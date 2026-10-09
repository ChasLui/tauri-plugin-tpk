//! Ask the platform not to back a directory up.
//!
//! One function, in its own crate for one reason: on Apple platforms the call
//! is `unsafe`, and `tauri-plugin-tpk` is `#![forbid(unsafe_code)]`, which
//! cannot be relaxed locally. The unsafe block is here, alone, with nothing else
//! in the crate to review.
//!
//! On Apple platforms this sets `NSURLIsExcludedFromBackupKey`. That flag is a
//! property of a URL rather than of a file's contents, and Apple documents it
//! only as "the resource is excluded from all backups of app data" — nothing
//! about what happens to children.
//!
//! What is *not* safe to assume: that a file created inside an already-excluded
//! directory inherits the flag. Apple does not say so either way, and field
//! reports disagree. So the caller re-applies it after anything that adds files
//! to the directory. Re-applying is one syscall and setting the flag twice is
//! harmless.
//!
//! The path must already exist. Setting the flag on a missing directory fails,
//! which is why the caller creates its directories first.
//!
//! Everywhere else it is a no-op. Android's equivalent is
//! `<data-extraction-rules>` in the host app's manifest, and no library can
//! merge into that; the host declares it.
#![deny(missing_docs)]
#![deny(unsafe_code)]

use std::path::Path;

/// Ask the platform not to back `path` up.
///
/// Best effort by nature: the caller is expected to treat a failure as lost
/// backup quota, not as a reason to stop.
///
/// # Errors
///
/// Returns [`std::io::Error`] if the platform refused the request, or if the
/// path is not valid UTF-8 on a target that needs to hand it to Foundation. On
/// targets with no such flag this always returns `Ok(())`.
pub fn exclude_from_backup(path: &Path) -> std::io::Result<()> {
    imp(path)
}

#[cfg(not(target_vendor = "apple"))]
fn imp(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}

#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
fn imp(path: &Path) -> std::io::Result<()> {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSNumber, NSString, NSURLIsExcludedFromBackupKey, NSURL};

    let Some(path) = path.to_str() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not valid UTF-8",
        ));
    };
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
    let excluded = NSNumber::numberWithBool(true);
    let excluded: &AnyObject = &excluded;

    // SAFETY: `setResourceValue:forKey:error:` is unsafe for one reason — it
    // takes an untyped value, and Foundation reads it as whatever the key
    // declares. `NSURLIsExcludedFromBackupKey` is documented to take a boolean,
    // and an `NSNumber` built from `true` is exactly that, so the value can
    // neither be misread nor read as the wrong size. The key itself is a
    // Foundation string constant initialised before `main`. Both objects
    // outlive the call.
    unsafe { url.setResourceValue_forKey_error(Some(excluded), NSURLIsExcludedFromBackupKey) }
        .map_err(|e| std::io::Error::other(format!("{}", &*e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_that_exists_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        // The flag is not read back: that is a second unsafe call, and the only
        // targets that have one are the ones CI does not run.
        exclude_from_backup(dir.path()).unwrap();
    }

    #[test]
    #[cfg(target_vendor = "apple")]
    fn a_path_that_does_not_exist_is_refused() {
        // Pins the ordering requirement the plugin depends on: the flag cannot
        // be set before the directory is created, so `Store::open` has to run
        // first. Apple-only, because everywhere else this is a no-op.
        let dir = tempfile::tempdir().unwrap();
        exclude_from_backup(&dir.path().join("not-created-yet")).unwrap_err();
    }

    #[test]
    #[cfg(not(target_vendor = "apple"))]
    fn there_is_nothing_to_do_off_apple() {
        // No flag to set, so not even a path that cannot exist is an error.
        exclude_from_backup(Path::new("/tpk/no/such/directory")).unwrap();
    }
}
