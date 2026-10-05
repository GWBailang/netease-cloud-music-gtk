use super::model::*;
use anyhow::{Result, anyhow, ensure};
use gtk::glib;
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
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
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
    ensure!(
        available_space(target)? >= fs::metadata(prepared)?.len(),
        "Not enough space in the save folder"
    );
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
        encoding: None,
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
            encoding: None,
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
                    encoding: None,
                    relative: lyric,
                    kind: FileKind::Lyrics,
                    fingerprint: fp,
                });
            }
        }
        let title = id3::Tag::read_from_path(target.root.join(&path))
            .ok()
            .and_then(|tag| {
                use id3::TagLike;
                let title = tag.title()?.trim();
                if title.is_empty() {
                    return None;
                }
                Some(match tag.artist().filter(|s| !s.trim().is_empty()) {
                    Some(artist) => format!("{artist} - {title}"),
                    None => title.to_owned(),
                })
            })
            .unwrap_or(stem);
        result.push(SavedSong {
            requested_basename: None,
            state: SavedState::Available,
            key: format!("{}:{}", target.id, path.display()),
            location: target.clone(),
            song: None,
            title,
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
    execute_delete_impl(
        preview,
        mode,
        |staged, item| secure_trash(staged, item),
        |_| Ok(()),
    )
}
pub fn execute_delete_with_trash(
    preview: &DeletePreview,
    mode: DeleteMode,
    trash: impl Fn(&Path) -> Result<()>,
) -> DeleteReport {
    execute_delete_impl(preview, mode, |staged, _| trash(staged), |_| Ok(()))
}
fn execute_delete_impl(
    preview: &DeletePreview,
    mode: DeleteMode,
    trash: impl Fn(&Path, &DeleteItem) -> Result<()>,
    before_stage: impl Fn(&DeleteItem) -> Result<()>,
) -> DeleteReport {
    let results = preview
        .items
        .iter()
        .map(|item| {
            let result = (|| -> Result<()> {
                let (parent, name) = open_parent(&item.location, &item.file.relative, false)?;
                ensure!(
                    fingerprint(&item.location, &item.file.relative)? == item.file.fingerprint,
                    "File has changed; refresh and review it again"
                );
                before_stage(item)?;
                let (current, _) = open_parent(&item.location, &item.file.relative, false)?;
                ensure!(
                    current.metadata()?.ino() == parent.metadata()?.ino(),
                    "Directory has changed"
                );
                // Move into a private directory using the verified parent descriptor. Recheck
                // the actual moved file before any destructive operation, never a reopened path.
                let staging_name = format!(".ncm-delete-{}", fastrand::u64(..));
                rfs::mkdirat(&parent, &staging_name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
                let staging: File = rfs::openat(
                    &parent,
                    &staging_name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into();
                let moved = (|| -> Result<()> {
                    ensure!(
                        staging.metadata()?.mode() & 0o022 == 0
                            && staging.metadata()?.uid() == rustix::process::geteuid().as_raw(),
                        "Deletion staging is writable by another user"
                    );
                    rfs::renameat_with(&parent, &name, &staging, &name, RenameFlags::NOREPLACE)?;
                    let file: File = rfs::openat(
                        &staging,
                        &name,
                        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?
                    .into();
                    ensure!(
                        fingerprint_file(file)? == item.file.fingerprint,
                        "File was replaced during deletion"
                    );
                    match mode {
                        DeleteMode::Permanent => rfs::unlinkat(&staging, &name, AtFlags::empty())?,
                        DeleteMode::Trash => trash(
                            &Path::new(&format!("/proc/self/fd/{}", staging.as_raw_fd()))
                                .join(&name),
                            item,
                        )?,
                    }
                    let _ = staging.sync_all();
                    let _ = parent.sync_all();
                    Ok(())
                })();
                if let Err(error) = moved {
                    // A failed trash/revalidation must never erase a file or overwrite a new copy.
                    let restored =
                        rfs::renameat_with(&staging, &name, &parent, &name, RenameFlags::NOREPLACE);
                    let empty = rfs::unlinkat(&parent, &staging_name, AtFlags::REMOVEDIR).is_ok();
                    if restored.is_err() && !empty {
                        return Err(anyhow!(
                            "{error}; preserved in {}/{}",
                            item.location
                                .root
                                .join(item.file.relative.parent().unwrap_or(Path::new("")))
                                .join(&staging_name)
                                .display(),
                            name.to_string_lossy()
                        ));
                    }
                    return Err(error);
                }
                let _ = rfs::unlinkat(&parent, &staging_name, AtFlags::REMOVEDIR);
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

// Descriptor-based Freedesktop Trash: GIO's path-based move would reopen a
// mutable parent. Preserve the original path in .trashinfo and rename the staged
// inode directly into the same filesystem's Trash instead.
fn secure_trash(staged: &Path, item: &DeleteItem) -> Result<()> {
    let data = glib::user_data_dir();
    fs::create_dir_all(&data)?;
    let home = fs::metadata(&data)?.dev() == item.location.filesystem_id;
    let mut top = item.location.root.clone();
    if !home {
        while let Some(parent) = top.parent() {
            if fs::metadata(parent)?.dev() != item.location.filesystem_id {
                break;
            }
            top = parent.to_path_buf();
        }
    }
    let uid = rustix::process::geteuid().as_raw();
    let trash_path = if home {
        data.join("Trash")
    } else {
        let common = top.join(".Trash");
        if fs::symlink_metadata(&common)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && m.mode() & 0o1000 != 0)
        {
            common.join(uid.to_string())
        } else {
            top.join(format!(".Trash-{uid}"))
        }
    };
    let original = item.location.root.join(&item.file.relative);
    let path = if home {
        original.as_path()
    } else {
        original.strip_prefix(&top)?
    };
    secure_trash_at(staged, &trash_path, path)
}
fn secure_trash_at(staged: &Path, trash_path: &Path, original: &Path) -> Result<()> {
    let uid = rustix::process::geteuid().as_raw();
    let base = TargetLocation::capture(
        trash_path
            .parent()
            .ok_or_else(|| anyhow!("Trash unavailable"))?,
        false,
        None,
        None,
    )?;
    let dir_name = trash_path
        .file_name()
        .ok_or_else(|| anyhow!("Trash unavailable"))?;
    let (parent, _) = open_parent(&base, Path::new(dir_name), false)?;
    match rfs::mkdirat(&parent, dir_name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(e.into()),
    }
    let trash: File = rfs::openat(
        &parent,
        dir_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into();
    ensure!(
        trash.metadata()?.uid() == uid && trash.metadata()?.mode() & 0o077 == 0,
        "Trash directory is not private"
    );
    let child = |name: &str| -> Result<File> {
        match rfs::mkdirat(&trash, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        let file: File = rfs::openat(
            &trash,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        ensure!(
            file.metadata()?.uid() == uid && file.metadata()?.mode() & 0o077 == 0,
            "Trash subdirectory is not private"
        );
        Ok(file)
    };
    let files = child("files")?;
    let info = child("info")?;
    let unique = format!(
        "ncm-{}-{}",
        fastrand::u64(..),
        original.file_name().unwrap_or_default().to_string_lossy()
    );
    let info_name = format!("{unique}.trashinfo");
    let mut record: File = rfs::openat(
        &info,
        &info_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?
    .into();
    let path = original;
    use std::os::unix::ffi::OsStrExt;
    let encoded: String = path
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(b) {
                (*b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let result = (|| -> Result<()> {
        writeln!(
            record,
            "[Trash Info]\nPath={encoded}\nDeletionDate={}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
        )?;
        record.sync_all()?;
        let source: File = File::open(
            staged
                .parent()
                .ok_or_else(|| anyhow!("Missing staged parent"))?,
        )?;
        rfs::renameat_with(
            &source,
            staged.file_name().unwrap(),
            &files,
            &unique,
            RenameFlags::NOREPLACE,
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rfs::unlinkat(&info, &info_name, AtFlags::empty());
    }
    result?;
    // Rename is the commit point; post-commit fsync failure must not cause a restore
    // or report the file as still present at the original location.
    let _ = files.sync_all();
    let _ = info.sync_all();
    let _ = trash.sync_all();
    Ok(())
}

#[cfg(test)]
mod deletion_race_tests {
    use super::*;
    #[test]
    fn trash_moves_verified_inode_and_records_original_path() {
        let root = std::env::temp_dir().join(format!("ncm-trash-{}", fastrand::u64(..)));
        fs::create_dir(&root).unwrap();
        let path = root.join("Song 中文.mp3");
        fs::write(&path, b"reviewed audio").unwrap();
        let target = TargetLocation::capture(&root, false, None, None).unwrap();
        let preview = preview_delete(&scan(&target).unwrap(), true, true).unwrap();
        let report = execute_delete_impl(
            &preview,
            DeleteMode::Trash,
            |staged, _| secure_trash_at(staged, &root.join("Trash"), &path),
            |_| Ok(()),
        );
        assert!(
            report.results[0].error.is_none(),
            "{:?}",
            report.results[0].error
        );
        assert!(!path.exists());
        let contents = fs::read_dir(root.join("Trash/files"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(contents).unwrap(), b"reviewed audio");
        let info = fs::read_dir(root.join("Trash/info"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = fs::read_to_string(info).unwrap();
        assert!(text.contains("Song%20%E4%B8%AD%E6%96%87.mp3") && text.contains("DeletionDate="));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn permanent_delete_preserves_replacement_between_verification_and_staging() {
        let root = std::env::temp_dir().join(format!("ncm-delete-race-{}", fastrand::u64(..)));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("song.mp3"), b"original").unwrap();
        let target = TargetLocation::capture(&root, false, None, None).unwrap();
        let preview = preview_delete(&scan(&target).unwrap(), true, true).unwrap();
        let report = execute_delete_impl(
            &preview,
            DeleteMode::Permanent,
            |_, _| Ok(()),
            |_| {
                fs::rename(root.join("song.mp3"), root.join("old.mp3"))?;
                fs::write(root.join("song.mp3"), b"replacement")?;
                Ok(())
            },
        );
        assert!(report.results[0].error.is_some());
        assert_eq!(fs::read(root.join("song.mp3")).unwrap(), b"replacement");
        fs::remove_dir_all(root).unwrap();
    }
}

/// Remove only a specific journalled temporary name from a validated location.
pub fn cleanup_temporary(target: &TargetLocation, relative: &Path) -> Result<()> {
    let name = relative
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Invalid temporary path"))?;
    ensure!(
        name.strip_prefix(".ncm-")
            .and_then(|s| s.strip_suffix(".part"))
            .is_some_and(|s| s.parse::<u64>().is_ok()),
        "Not an owned temporary filename"
    );
    let (parent, name) = open_parent(target, relative, false)?;
    match rfs::openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(fd) => {
            let file: File = fd.into();
            ensure!(
                file.metadata()?.is_file(),
                "Temporary path is not a regular file"
            );
        }
    }
    rfs::unlinkat(&parent, &name, AtFlags::empty())?;
    parent.sync_all()?;
    Ok(())
}

/// Recover only an inode-identified work directory recorded before media writes.
/// Unknown files or nested directories are retained for manual inspection.
pub fn cleanup_work_directory(target: &TargetLocation) -> Result<()> {
    if target
        .root
        .symlink_metadata()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    validate_location(target)?;
    let name = target
        .root
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Invalid work directory"))?;
    let parts = name.split('-').collect::<Vec<_>>();
    ensure!(
        parts.len() == 3
            && parts[0] == "task"
            && parts[1].parse::<u64>().is_ok()
            && parts[2].parse::<u64>().is_ok(),
        "Not an owned work directory"
    );
    let (directory, _) = open_parent(target, Path::new(".cleanup"), false)?;
    let entries = fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    for entry in &entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let known = matches!(name.as_ref(), "source" | "audio.mp3")
            || ["lyrics", "prepared"].iter().any(|prefix| {
                name.strip_prefix(prefix).is_some_and(|suffix| {
                    [
                        ".mp3",
                        ".lrc",
                        ".txt",
                        ".srt",
                        ".translated.lrc",
                        ".translated.txt",
                        ".translated.srt",
                    ]
                    .contains(&suffix)
                })
            });
        ensure!(
            known && entry.file_type()?.is_file(),
            "Work directory contains an unrecognized file; retained for inspection"
        );
    }
    for entry in entries {
        rfs::unlinkat(&directory, entry.file_name(), AtFlags::empty())?;
    }
    directory.sync_all()?;
    let parent_path = target
        .root
        .parent()
        .ok_or_else(|| anyhow!("Missing work parent"))?;
    let parent_target = TargetLocation::capture(parent_path, false, None, None)?;
    let (parent, name) = open_parent(&parent_target, Path::new(name), false)?;
    // Check the directory identity once more before removing the empty directory.
    validate_location(target)?;
    rfs::unlinkat(&parent, &name, AtFlags::REMOVEDIR)?;
    parent.sync_all()?;
    Ok(())
}
