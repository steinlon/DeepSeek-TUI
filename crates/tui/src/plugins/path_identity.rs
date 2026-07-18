use std::path::Path;

use sha2::Digest;

/// Add a lossless, platform-scoped OS path to a digest.
///
/// Plugin identity must never pass through Unicode replacement. Unix paths
/// are byte strings and Windows paths are UTF-16 strings; two distinct native
/// paths can therefore have the same `to_string_lossy()` representation. The
/// framing below also prevents a future platform/domain change from silently
/// reusing an existing trust receipt.
pub(crate) fn hash_os_path(hasher: &mut impl Digest, domain: &'static [u8], path: &Path) {
    hasher.update(b"codewhale-os-path-v1\0");
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let bytes = path.as_os_str().as_bytes();
        hasher.update(b"unix-bytes\0");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        hasher.update(b"windows-utf16le\0");
        hasher.update((units.len() as u64).to_le_bytes());
        for unit in units {
            hasher.update(unit.to_le_bytes());
        }
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        // `as_encoded_bytes` is lossless for the platform's `OsStr`
        // representation within one Rust implementation. Keep this fallback
        // separately tagged so receipts can never cross into Unix/Windows.
        let bytes = path.as_os_str().as_encoded_bytes();
        hasher.update(b"rust-osstr-encoded\0");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Sha256;

    fn digest(path: &Path) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hash_os_path(&mut hasher, b"test-domain", path);
        hasher.finalize().to_vec()
    }

    #[cfg(unix)]
    #[test]
    fn invalid_unicode_paths_do_not_collapse_to_replacement_text() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let first = OsString::from_vec(vec![b'a', 0xff]);
        let second = OsString::from_vec(vec![b'a', 0xfe]);
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(digest(Path::new(&first)), digest(Path::new(&second)));
    }

    #[cfg(windows)]
    #[test]
    fn unpaired_utf16_paths_do_not_collapse_to_replacement_text() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;

        let first = OsString::from_wide(&[b'a' as u16, 0xd800]);
        let second = OsString::from_wide(&[b'a' as u16, 0xd801]);
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(digest(Path::new(&first)), digest(Path::new(&second)));
    }
}
