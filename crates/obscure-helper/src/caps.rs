//! File capabilities written and read through an open descriptor
//! (`security.capability` xattr, `vfs_cap_data` revision 2), so no external
//! `setcap`/`getcap` is looked up in `PATH` and the target cannot change
//! between validation and the write.

use std::fs::File;
use std::os::fd::AsRawFd;

const XATTR: &std::ffi::CStr = c"security.capability";
const VFS_CAP_REVISION_2: u32 = 0x0200_0000;
const VFS_CAP_REVISION_MASK: u32 = 0xFF00_0000;
const VFS_CAP_FLAGS_EFFECTIVE: u32 = 0x0000_0001;

const CAP_NET_BIND_SERVICE: u32 = 10;
const CAP_NET_ADMIN: u32 = 12;
const CAP_NET_RAW: u32 = 13;

/// What Xray's `tun` inbound needs (`cap_net_admin,cap_net_raw,
/// cap_net_bind_service+ep`; binding the DNS port needs the last one).
pub const TUN_CAPS: &[u32] = &[CAP_NET_ADMIN, CAP_NET_RAW, CAP_NET_BIND_SERVICE];

/// Serialises a permitted+effective capability set as `vfs_cap_data` v2.
pub fn encode(caps: &[u32]) -> [u8; 20] {
    let mut permitted = [0u32; 2];
    for &c in caps {
        permitted[(c / 32) as usize] |= 1 << (c % 32);
    }
    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&(VFS_CAP_REVISION_2 | VFS_CAP_FLAGS_EFFECTIVE).to_le_bytes());
    out[4..8].copy_from_slice(&permitted[0].to_le_bytes());
    // inheritable[0] stays 0
    out[12..16].copy_from_slice(&permitted[1].to_le_bytes());
    // inheritable[1] stays 0
    out
}

/// Parses `vfs_cap_data` (v2 or v3) into `(permitted caps, effective)`.
pub fn decode(raw: &[u8]) -> Option<(Vec<u32>, bool)> {
    if raw.len() < 20 {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap());
    let magic = word(0);
    if magic & VFS_CAP_REVISION_MASK < VFS_CAP_REVISION_2 {
        return None;
    }
    let permitted = [word(4), word(12)];
    let caps = (0..64)
        .filter(|c| permitted[(c / 32) as usize] & (1 << (c % 32)) != 0)
        .collect();
    Some((caps, magic & VFS_CAP_FLAGS_EFFECTIVE != 0))
}

fn name(cap: u32) -> String {
    match cap {
        CAP_NET_BIND_SERVICE => "cap_net_bind_service".into(),
        CAP_NET_ADMIN => "cap_net_admin".into(),
        CAP_NET_RAW => "cap_net_raw".into(),
        n => format!("cap_{n}"),
    }
}

/// `getcap`-style text (`cap_a,cap_b=ep`), empty when there are none.
pub fn describe(raw: &[u8]) -> String {
    match decode(raw) {
        Some((caps, eff)) if !caps.is_empty() => {
            let names: Vec<String> = caps.into_iter().map(name).collect();
            format!("{}={}", names.join(","), if eff { "ep" } else { "p" })
        }
        _ => String::new(),
    }
}

/// Writes the capability set on the open file.
pub fn set(file: &File, caps: &[u32]) -> Result<(), String> {
    let data = encode(caps);
    // SAFETY: valid fd, NUL-terminated name, buffer of the given length.
    let r = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            XATTR.as_ptr(),
            data.as_ptr().cast(),
            data.len(),
            0,
        )
    };
    if r != 0 {
        return Err(format!(
            "cannot set capabilities: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Reads the raw capability xattr; empty when the file has none.
pub fn get(file: &File) -> Result<Vec<u8>, String> {
    let mut buf = [0u8; 64];
    // SAFETY: valid fd, NUL-terminated name, writable buffer of that size.
    let n = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            XATTR.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::ENODATA) => Ok(Vec::new()),
            _ => Err(format!("cannot read capabilities: {err}")),
        };
    }
    Ok(buf[..n as usize].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_setcap() {
        // `setcap cap_net_admin,cap_net_raw,cap_net_bind_service+ep` writes
        // magic 0x02000001 and permitted 0x3400.
        let raw = encode(TUN_CAPS);
        assert_eq!(
            raw,
            [
                0x01, 0, 0, 0x02, 0x00, 0x34, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(
            describe(&raw),
            "cap_net_bind_service,cap_net_admin,cap_net_raw=ep"
        );
        assert_eq!(describe(&[]), "");
    }

    #[test]
    fn decodes_v3_from_user_namespaces() {
        // v3 = v2 layout + 4-byte rootid.
        let mut raw = encode(&[CAP_NET_ADMIN]).to_vec();
        raw[3] = 0x03;
        raw.extend_from_slice(&1000u32.to_le_bytes());
        assert_eq!(decode(&raw), Some((vec![CAP_NET_ADMIN], true)));
    }
}
