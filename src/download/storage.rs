use super::model::*;
use anyhow::{Result, anyhow, ensure};
use gtk::{gio, glib, prelude::*};
use rustix::fs::{self as rfs, AtFlags, Mode, OFlags, RenameFlags};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
};

impl TargetLocation {
    pub fn capture(
        root: &Path,
        removable: bool,
        volume_uuid: Option<String>,
        mount_session: Option<String>,
    ) -> Result<Self> {
        let root = root.canonicalize()?;
        let metadata = fs::metadata(&root)?;
        ensure!(metadata.is_dir(), "Choose a directory");
        let id = format!("{}:{}:{}", metadata.dev(), metadata.ino(), root.display());
        Ok(Self {
            id,
            root,
            filesystem_id: metadata.dev(),
            root_inode: metadata.ino(),
            removable,
            volume_uuid,
            mount_session,
        })
    }
}
pub fn validate_location(target: &TargetLocation) -> Result<()> {
    let metadata = fs::symlink_metadata(&target.root)
        .map_err(|_| anyhow!("Save location is unavailable; select it again"))?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == target.filesystem_id
            && metadata.ino() == target.root_inode,
        "Save location has changed; select it again"
    );
    Ok(())
}
fn open_parent(target: &TargetLocation, relative: &Path, create: bool) -> Result<(File, OsString)> {
    validate_location(target)?;
    let parts = relative
        .components()
        .map(|c| match c {
            Component::Normal(s) => Ok(s.to_owned()),
            _ => Err(anyhow!("Unsafe relative file path")),
        })
        .collect::<Result<Vec<_>>>()?;
    let name = parts
        .last()
        .ok_or_else(|| anyhow!("Missing file name"))?
        .clone();
    let mut parent: File = rfs::open(
        &target.root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into();
    let metadata = parent.metadata()?;
    ensure!(
        metadata.dev() == target.filesystem_id && metadata.ino() == target.root_inode,
        "Save location has changed"
    );
    for part in &parts[..parts.len() - 1] {
        if create {
            match rfs::mkdirat(
                &parent,
                part,
                Mode::RUSR
                    | Mode::WUSR
                    | Mode::XUSR
                    | Mode::RGRP
                    | Mode::XGRP
                    | Mode::ROTH
                    | Mode::XOTH,
            ) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
        }
        parent = rfs::openat(
            &parent,
            part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        ensure!(
            parent.metadata()?.dev() == target.filesystem_id,
            "Refusing to cross a mounted file system"
        );
    }
    Ok((parent, name))
}
fn fingerprint_file(mut file: File) -> Result<Fingerprint> {
    let before = file.metadata()?;
    ensure!(before.is_file(), "Not a regular file");
    let mut checksum = glib::Checksum::new(glib::ChecksumType::Sha256)
        .ok_or_else(|| anyhow!("Checksum unavailable"))?;
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        checksum.update(&buf[..n]);
    }
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec(),
        "File changed while reading"
    );
    Ok(Fingerprint {
        size: after.len(),
        sha256: checksum.string().unwrap_or_default(),
        inode: after.ino(),
        modified_ns: after.mtime().max(0) as u128 * 1_000_000_000
            + after.mtime_nsec().max(0) as u128,
    })
}
pub fn fingerprint(target: &TargetLocation, relative: &Path) -> Result<Fingerprint> {
    let (parent, name) = open_parent(target, relative, false)?;
    let file = rfs::openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into();
    fingerprint_file(file)
}
pub fn commit_file(
    prepared: &Path,
    target: &TargetLocation,
    relative: &Path,
    cancelled: &AtomicBool,
) -> Result<StoredFile> {
    commit_file_tracked(prepared, target, relative, cancelled, |_, _| Ok(()))
}
pub fn commit_file_tracked(
    prepared: &Path,
    target: &TargetLocation,
    relative: &Path,
    cancelled: &AtomicBool,
    track: impl Fn(&Path, bool) -> Result<()>,
) -> Result<StoredFile> {
    ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
    let (parent, name) = open_parent(target, relative, true)?;
    let temporary = format!(".ncm-{}.part", fastrand::u64(..));
    let temporary_relative = relative.with_file_name(&temporary);
    track(&temporary_relative, true)?;
    let mut output: File = rfs::openat(
        &parent,
        &temporary,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
    )?
    .into();
    let result = (|| -> Result<()> {
        let mut input = File::open(prepared)?;
        let mut buf = [0; 65536];
        loop {
            ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
            validate_location(target)?;
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
        }
        output.sync_all()?;
        ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
        validate_location(target)?;
        // Reopen the parent from the root to detect a directory replaced during the copy.
        let (current, _) = open_parent(target, relative, false)?;
        ensure!(
            current.metadata()?.ino() == parent.metadata()?.ino(),
            "Destination directory changed"
        );
        rfs::renameat_with(&parent, &temporary, &parent, &name, RenameFlags::NOREPLACE)?;
        parent.sync_all()?;
        Ok(())
    })();
    drop(output);
    if result.is_err() {
        if rfs::unlinkat(&parent, &temporary, AtFlags::empty()).is_ok() {
            track(&temporary_relative, false)?;
        }
    } else {
        track(&temporary_relative, false)?;
    }
    result?;
    let kind = if relative
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("mp3"))
    {
        FileKind::Audio
    } else {
        FileKind::Lyrics
    };
    Ok(StoredFile {
        relative: relative.into(),
        kind,
        fingerprint: fingerprint(target, relative)?,
    })
}
pub fn available_space(target: &TargetLocation) -> Result<u64> {
    validate_location(target)?;
    let info = rfs::statvfs(&target.root)?;
    Ok(info.f_bavail.saturating_mul(info.f_frsize))
}
pub fn scan(target: &TargetLocation) -> Result<Vec<SavedSong>> {
    validate_location(target)?;
    let mut result = Vec::new();
    scan_dir(target, Path::new(""), &mut result)?;
    Ok(result)
}
fn scan_dir(target: &TargetLocation, relative: &Path, result: &mut Vec<SavedSong>) -> Result<()> {
    validate_location(target)?;
    let (directory, _) = open_parent(target, &relative.join(".scan"), false)?;
    let mut entries = fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let path = relative.join(entry.file_name());
        if kind.is_dir() {
            let (fd, _) = open_parent(target, &path.join("dummy"), false)?;
            drop(fd);
            scan_dir(target, &path, result)?;
            continue;
        }
        if !kind.is_file()
            || !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("mp3"))
        {
            continue;
        }
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut files = vec![StoredFile {
            relative: path.clone(),
            kind: FileKind::Audio,
            fingerprint: fingerprint(target, &path)?,
        }];
        for suffix in [
            "lrc",
            "txt",
            "srt",
            "translated.lrc",
            "translated.txt",
            "translated.srt",
        ] {
            let lyric = path.with_file_name(format!("{stem}.{suffix}"));
            if let Ok(fp) = fingerprint(target, &lyric) {
                files.push(StoredFile {
                    relative: lyric,
                    kind: FileKind::Lyrics,
                    fingerprint: fp,
                });
            }
        }
        result.push(SavedSong {
            key: format!("{}:{}", target.id, path.display()),
            location: target.clone(),
            song: None,
            title: stem,
            bitrate: 0,
            encoding: TextEncoding::Utf8,
            lyric_mode: LyricMode::Original,
            files,
            imported: true,
        });
    }
    Ok(())
}
pub fn preview_delete(
    items: &[SavedSong],
    include_audio: bool,
    include_lyrics: bool,
) -> Result<DeletePreview> {
    let mut preview = DeletePreview::default();
    let mut seen = HashSet::new();
    for song in items {
        for file in &song.files {
            if (file.kind == FileKind::Audio && !include_audio)
                || (file.kind == FileKind::Lyrics && !include_lyrics)
            {
                continue;
            }
            ensure!(
                fingerprint(&song.location, &file.relative)? == file.fingerprint,
                "File has changed; refresh and review it again"
            );
            if seen.insert((song.location.id.clone(), file.relative.clone())) {
                preview.total_bytes += file.fingerprint.size;
                preview.items.push(DeleteItem {
                    key: song.key.clone(),
                    location: song.location.clone(),
                    file: file.clone(),
                });
            }
        }
    }
    Ok(preview)
}
pub fn execute_delete(preview: &DeletePreview, mode: DeleteMode) -> DeleteReport {
    execute_delete_with_trash(preview, mode, |path| {
        gio::File::for_path(path).trash(None::<&gio::Cancellable>)?;
        Ok(())
    })
}
pub fn execute_delete_with_trash(
    preview: &DeletePreview,
    mode: DeleteMode,
    trash: impl Fn(&Path) -> Result<()>,
) -> DeleteReport {
    let results = preview
        .items
        .iter()
        .map(|item| {
            let result = (|| -> Result<()> {
                ensure!(
                    fingerprint(&item.location, &item.file.relative)? == item.file.fingerprint,
                    "File has changed; refresh and review it again"
                );
                let (parent, name) = open_parent(&item.location, &item.file.relative, false)?;
                match mode {
                    DeleteMode::Trash => trash(&item.location.root.join(&item.file.relative))?,
                    DeleteMode::Permanent => {
                        rfs::unlinkat(&parent, &name, AtFlags::empty())?;
                    }
                }
                Ok(())
            })();
            DeleteResult {
                item: item.clone(),
                error: result.err().map(|e| e.to_string()),
            }
        })
        .collect();
    DeleteReport { results }
}
