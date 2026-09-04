//! Removable-media identification (`docs/requirements.md` §10.4/§10.21). §10.4 defines
//! only the cross-platform *abstraction* — an OS-swappable identifier lookup, storing
//! results in `removable_media`'s OS-agnostic `identifier_type`/`identifier_value`
//! columns — and explicitly leaves "which identifier, in what priority, per OS" to be
//! decided at each OS's own implementation time. This gives Linux (the only OS this
//! development environment can actually exercise and validate against real `lsblk`
//! output) a real backend; Windows and macOS get a documented stub that reports no
//! connected media until someone implements and validates a backend on that OS. A stub
//! is deliberately safer than a guess: a backend that invents an identifier could
//! silently misidentify one physical medium as another (§6's reuse-without-reconnect
//! promise depends on this key never lying), whereas reporting nothing just falls
//! through to §10.21's manual-label fallback.

use std::io;
use std::path::PathBuf;

/// One removable medium this backend was able to identify, currently connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedMedia {
    pub identifier_type: String,
    pub identifier_value: String,
    pub display_name: Option<String>,
    pub mount_path: PathBuf,
}

pub trait MediaIdentifier {
    /// Lists every currently-connected removable medium this backend can identify.
    /// Media this backend can see but can't derive a trustworthy identifier for are
    /// simply omitted — callers fall back to §10.21's manual label for those.
    fn list_connected(&self) -> io::Result<Vec<DetectedMedia>>;
}

/// The `removable_media.platform` value for the OS this binary is running on (§10.12's
/// `CHECK (platform IN ('windows','macos','linux'))`).
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Selects the real backend for the OS this binary is running on.
pub fn platform_identifier() -> Box<dyn MediaIdentifier> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxMediaIdentifier)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(UnimplementedMediaIdentifier)
    }
}

/// See the module doc: no backend has been implemented and validated for this OS yet.
/// Only ever constructed on non-Linux targets; `#[allow(dead_code)]` keeps a Linux
/// build (where the other branch of `platform_identifier` is compiled out) warning-free.
#[allow(dead_code)]
struct UnimplementedMediaIdentifier;

impl MediaIdentifier for UnimplementedMediaIdentifier {
    fn list_connected(&self) -> io::Result<Vec<DetectedMedia>> {
        Ok(Vec::new())
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxMediaIdentifier;

#[cfg(target_os = "linux")]
impl MediaIdentifier for LinuxMediaIdentifier {
    fn list_connected(&self) -> io::Result<Vec<DetectedMedia>> {
        use std::process::Command;

        // `lsblk` missing or failing degrades to "nothing identifiable" (falling
        // through to §10.21's manual-label prompt) rather than a hard error — the tool
        // not being installed on this particular Linux system/container shouldn't
        // block `scan media` outright.
        let Ok(output) = Command::new("lsblk")
            .args(["-J", "-o", "NAME,SERIAL,UUID,MOUNTPOINT,RM"])
            .output()
        else {
            return Ok(Vec::new());
        };
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(parse_lsblk_json(&String::from_utf8_lossy(&output.stdout)))
    }
}

// `LsblkOutput`/`LsblkDevice`/`parse_lsblk_json`/`walk_lsblk_device` below are only
// ever called from `LinuxMediaIdentifier` (Linux-only) or from this module's tests
// (which run on every OS in CI, per the doc comment above `parse_lsblk_json`). On a
// non-Linux, non-test build there is no caller at all, so `#[allow(dead_code)]` is
// needed to keep the parser OS-independent without gating it behind `cfg(target_os =
// "linux")` and losing cross-platform test coverage of the parsing logic itself.
#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct LsblkOutput {
    blockdevices: Vec<LsblkDevice>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct LsblkDevice {
    name: String,
    serial: Option<String>,
    uuid: Option<String>,
    mountpoint: Option<String>,
    rm: Option<bool>,
    #[serde(default)]
    children: Vec<LsblkDevice>,
}

/// Parses `lsblk -J -o NAME,SERIAL,UUID,MOUNTPOINT,RM` output into detected media. Pure
/// and OS-independent (unlike `LinuxMediaIdentifier` itself, which shells out), so it's
/// exercised on every OS in CI even though only Linux actually calls it at runtime.
///
/// A removable top-level disk's serial applies to all of its mounted partitions (a
/// partition itself has no serial of its own); a partition's own filesystem UUID is
/// used only when no ancestor serial is available. Per §10.4, device serial is
/// preferred as the more stable, device-level identifier.
#[allow(dead_code)]
fn parse_lsblk_json(json: &str) -> Vec<DetectedMedia> {
    let Ok(parsed) = serde_json::from_str::<LsblkOutput>(json) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for dev in &parsed.blockdevices {
        walk_lsblk_device(dev, false, None, &mut found);
    }
    found
}

#[allow(dead_code)]
fn walk_lsblk_device(
    dev: &LsblkDevice,
    ancestor_removable: bool,
    ancestor_serial: Option<&str>,
    out: &mut Vec<DetectedMedia>,
) {
    let removable = ancestor_removable || dev.rm.unwrap_or(false);
    let serial = dev.serial.as_deref().or(ancestor_serial);

    if removable {
        if let Some(mountpoint) = &dev.mountpoint {
            let identifier = serial
                .map(|s| ("device_serial", s.to_string()))
                .or_else(|| {
                    dev.uuid
                        .as_deref()
                        .map(|u| ("filesystem_uuid", u.to_string()))
                });
            if let Some((identifier_type, identifier_value)) = identifier {
                out.push(DetectedMedia {
                    identifier_type: identifier_type.to_string(),
                    identifier_value,
                    display_name: Some(dev.name.clone()),
                    mount_path: PathBuf::from(mountpoint),
                });
            }
            // No serial and no UUID: this mounted device can't be trustworthily
            // identified. Omitted here, not pushed with a guessed value — the caller
            // (CLI `scan media`) falls back to §10.21's manual label for it.
        }
    }

    for child in &dev.children {
        walk_lsblk_device(child, removable, serial, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_disk_serial_over_partition_uuid_for_a_removable_usb_drive() {
        let json = r#"{
            "blockdevices": [
                {
                    "name": "sda",
                    "serial": "USB1234SERIAL",
                    "uuid": null,
                    "mountpoint": null,
                    "rm": true,
                    "children": [
                        {
                            "name": "sda1",
                            "serial": null,
                            "uuid": "AAAA-BBBB",
                            "mountpoint": "/media/user/USBDRIVE",
                            "rm": true
                        }
                    ]
                }
            ]
        }"#;

        let media = parse_lsblk_json(json);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].identifier_type, "device_serial");
        assert_eq!(media[0].identifier_value, "USB1234SERIAL");
        assert_eq!(media[0].mount_path, PathBuf::from("/media/user/USBDRIVE"));
    }

    #[test]
    fn falls_back_to_filesystem_uuid_when_no_serial_is_reported() {
        let json = r#"{
            "blockdevices": [
                {
                    "name": "sdb",
                    "serial": null,
                    "uuid": null,
                    "mountpoint": null,
                    "rm": true,
                    "children": [
                        {
                            "name": "sdb1",
                            "serial": null,
                            "uuid": "CCCC-DDDD",
                            "mountpoint": "/media/user/NOSERIAL",
                            "rm": true
                        }
                    ]
                }
            ]
        }"#;

        let media = parse_lsblk_json(json);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].identifier_type, "filesystem_uuid");
        assert_eq!(media[0].identifier_value, "CCCC-DDDD");
    }

    #[test]
    fn ignores_non_removable_and_unmounted_devices() {
        let json = r#"{
            "blockdevices": [
                {
                    "name": "nvme0n1",
                    "serial": "INTERNAL_SSD",
                    "uuid": null,
                    "mountpoint": null,
                    "rm": false,
                    "children": [
                        {
                            "name": "nvme0n1p1",
                            "serial": null,
                            "uuid": "EEEE-FFFF",
                            "mountpoint": "/",
                            "rm": false
                        }
                    ]
                },
                {
                    "name": "sdc",
                    "serial": "USB_UNMOUNTED",
                    "uuid": null,
                    "mountpoint": null,
                    "rm": true,
                    "children": [
                        {
                            "name": "sdc1",
                            "serial": null,
                            "uuid": null,
                            "mountpoint": null,
                            "rm": true
                        }
                    ]
                }
            ]
        }"#;

        assert!(parse_lsblk_json(json).is_empty());
    }

    #[test]
    fn omits_a_removable_mounted_device_with_neither_serial_nor_uuid() {
        let json = r#"{
            "blockdevices": [
                {
                    "name": "sdd",
                    "serial": null,
                    "uuid": null,
                    "mountpoint": null,
                    "rm": true,
                    "children": [
                        {
                            "name": "sdd1",
                            "serial": null,
                            "uuid": null,
                            "mountpoint": "/media/user/UNIDENTIFIABLE",
                            "rm": true
                        }
                    ]
                }
            ]
        }"#;

        // No usable identifier at all: omitted, not guessed — §10.21's manual-label
        // fallback is the caller's job for this one.
        assert!(parse_lsblk_json(json).is_empty());
    }

    #[test]
    fn malformed_json_yields_no_media_rather_than_an_error() {
        assert!(parse_lsblk_json("not json at all").is_empty());
    }
}
