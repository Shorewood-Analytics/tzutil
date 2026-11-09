use std::fs;
use std::path::Path;
use std::process::Command;

/// Detects the host's configured IANA timezone.
///
/// Tries sources in order:
/// 1. /etc/timezone (simple text file)
/// 2. /etc/localtime symlink target
/// 3. timedatectl show -p Timezone --value
/// 4. iana-time-zone crate (fallback; native equivalent of Python's tzlocal)
pub fn get_system_timezone() -> Result<String, String> {
    detect_from_etc_timezone(Path::new("/etc/timezone"))
        .or_else(|| detect_from_etc_localtime(Path::new("/etc/localtime")))
        .or_else(detect_from_timedatectl)
        .or_else(detect_from_iana_crate)
        .ok_or_else(|| "Could not determine system timezone from any source".to_string())
}

fn detect_from_etc_timezone(path: &Path) -> Option<String> {
    let tz = fs::read_to_string(path).ok()?.trim().to_string();
    is_valid_iana_tz(&tz).then_some(tz)
}

/// Upper bound on symlink hops followed from /etc/localtime (matches Linux's
/// MAXSYMLINKS), so a link loop terminates instead of spinning forever.
const MAX_SYMLINK_HOPS: usize = 40;

/// Follows the /etc/localtime symlink chain and returns the zone name from the
/// first hop whose target is a valid zoneinfo path. Stopping at the first such
/// hop (rather than canonicalizing) keeps aliases like `US/Mountain` intact.
fn detect_from_etc_localtime(path: &Path) -> Option<String> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        let target = fs::read_link(&current).ok()?;
        if let Some(tz) = extract_tz_from_zoneinfo_path(&target).filter(|tz| is_valid_iana_tz(tz)) {
            return Some(tz);
        }
        // Relative targets are relative to the directory holding the link.
        current = match current.parent() {
            Some(dir) => dir.join(&target),
            None => target,
        };
    }
    None
}

/// Extracts the IANA timezone name from a zoneinfo path, e.g.
/// `/usr/share/zoneinfo/America/Denver` -> `America/Denver`.
///
/// A leading `posix/` directory is dropped: `posix/` zones are the plain
/// zones under another path, and `posix/America/Denver` is not an IANA ID.
/// `right/` (leap-second) zones are kept as-is on purpose: they keep a
/// different clock, so stripping them would shift `TZ` by the leap seconds.
fn extract_tz_from_zoneinfo_path(path: &Path) -> Option<String> {
    const MARKER: &str = "/zoneinfo/";
    let path_str = path.to_str()?;
    let idx = path_str.find(MARKER)?;
    let tz = &path_str[idx + MARKER.len()..];
    let tz = tz.strip_prefix("posix/").unwrap_or(tz);
    (!tz.is_empty()).then(|| tz.to_string())
}

fn detect_from_timedatectl() -> Option<String> {
    let output = Command::new("timedatectl")
        .args(["show", "-p", "Timezone", "--value"])
        .output()
        .ok()?;
    parse_timedatectl_output(output.status.success(), &output.stdout)
}

fn parse_timedatectl_output(success: bool, stdout: &[u8]) -> Option<String> {
    if !success {
        return None;
    }
    let tz = String::from_utf8_lossy(stdout).trim().to_string();
    is_valid_iana_tz(&tz).then_some(tz)
}

fn detect_from_iana_crate() -> Option<String> {
    let tz = iana_time_zone::get_timezone().ok()?;
    is_valid_iana_tz(&tz).then_some(tz)
}

/// Validates that a string matches IANA timezone format.
///
/// IANA timezones are typically Continent/City or have multiple parts.
/// Examples: America/Denver, Europe/London, UTC, Etc/UTC, Etc/GMT+5
fn is_valid_iana_tz(tz: &str) -> bool {
    fn is_valid_component(part: &str) -> bool {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
    }

    if tz.is_empty() {
        return false;
    }
    if tz.contains('/') {
        tz.split('/').all(is_valid_component)
    } else {
        is_valid_component(tz)
    }
}

fn main() {
    match get_system_timezone() {
        Ok(tz) => println!("{tz}"),
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    mod is_valid_iana_tz_tests {
        use super::*;

        #[test]
        fn valid_two_part_timezone() {
            assert!(is_valid_iana_tz("America/Denver"));
            assert!(is_valid_iana_tz("Europe/London"));
            assert!(is_valid_iana_tz("Asia/Tokyo"));
        }

        #[test]
        fn valid_three_part_timezone() {
            assert!(is_valid_iana_tz("America/Argentina/Buenos_Aires"));
            assert!(is_valid_iana_tz("America/Indiana/Indianapolis"));
        }

        #[test]
        fn valid_single_part_timezone() {
            assert!(is_valid_iana_tz("UTC"));
            assert!(is_valid_iana_tz("GMT"));
        }

        #[test]
        fn timezone_with_underscores() {
            assert!(is_valid_iana_tz("America/Los_Angeles"));
            assert!(is_valid_iana_tz("America/New_York"));
        }

        #[test]
        fn timezone_with_hyphen_and_plus() {
            assert!(is_valid_iana_tz("Etc/GMT+5"));
        }

        #[test]
        fn invalid_empty_string() {
            assert!(!is_valid_iana_tz(""));
        }

        #[test]
        fn invalid_with_spaces() {
            assert!(!is_valid_iana_tz("America/ Denver"));
        }

        #[test]
        fn invalid_special_characters() {
            assert!(!is_valid_iana_tz("America/Den@ver"));
        }
    }

    mod detect_from_etc_timezone_tests {
        use super::*;

        #[test]
        fn reads_and_trims_valid_contents() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("timezone");
            fs::write(&path, "America/Denver\n").unwrap();
            assert_eq!(
                detect_from_etc_timezone(&path),
                Some("America/Denver".to_string())
            );
        }

        #[test]
        fn trims_surrounding_whitespace() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("timezone");
            fs::write(&path, "  Europe/London  \n").unwrap();
            assert_eq!(
                detect_from_etc_timezone(&path),
                Some("Europe/London".to_string())
            );
        }

        #[test]
        fn missing_file_returns_none() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("does-not-exist");
            assert_eq!(detect_from_etc_timezone(&path), None);
        }

        #[test]
        fn invalid_contents_returns_none() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("timezone");
            fs::write(&path, "not a timezone!!").unwrap();
            assert_eq!(detect_from_etc_timezone(&path), None);
        }
    }

    mod zoneinfo_path_tests {
        use super::*;

        #[test]
        fn extracts_two_part_zone() {
            let path = Path::new("/usr/share/zoneinfo/America/Los_Angeles");
            assert_eq!(
                extract_tz_from_zoneinfo_path(path),
                Some("America/Los_Angeles".to_string())
            );
        }

        #[test]
        fn extracts_three_part_zone() {
            let path = Path::new("/usr/share/zoneinfo/America/Argentina/Buenos_Aires");
            assert_eq!(
                extract_tz_from_zoneinfo_path(path),
                Some("America/Argentina/Buenos_Aires".to_string())
            );
        }

        #[test]
        fn missing_marker_returns_none() {
            let path = Path::new("/some/other/path");
            assert_eq!(extract_tz_from_zoneinfo_path(path), None);
        }

        #[test]
        fn symlink_target_is_resolved_end_to_end() {
            let dir = tempdir().unwrap();
            let target_dir = dir.path().join("usr/share/zoneinfo/America");
            fs::create_dir_all(&target_dir).unwrap();
            let target = target_dir.join("Los_Angeles");
            fs::write(&target, "fake tzdata").unwrap();

            let link = dir.path().join("localtime");
            symlink(&target, &link).unwrap();

            assert_eq!(
                detect_from_etc_localtime(&link),
                Some("America/Los_Angeles".to_string())
            );
        }

        #[test]
        fn posix_prefix_is_stripped() {
            assert_eq!(
                extract_tz_from_zoneinfo_path(Path::new(
                    "/usr/share/zoneinfo/posix/America/Denver"
                )),
                Some("America/Denver".to_string())
            );
            assert_eq!(
                extract_tz_from_zoneinfo_path(Path::new("../usr/share/zoneinfo/posix/Etc/UTC")),
                Some("Etc/UTC".to_string())
            );
        }

        #[test]
        fn right_prefix_is_kept() {
            assert_eq!(
                extract_tz_from_zoneinfo_path(Path::new(
                    "/usr/share/zoneinfo/right/America/Denver"
                )),
                Some("right/America/Denver".to_string())
            );
        }

        #[test]
        fn posix_like_names_are_not_mangled() {
            assert_eq!(
                extract_tz_from_zoneinfo_path(Path::new("/usr/share/zoneinfo/posixrules")),
                Some("posixrules".to_string())
            );
            assert_eq!(
                extract_tz_from_zoneinfo_path(Path::new("/usr/share/zoneinfo/posix/")),
                None
            );
        }

        #[test]
        fn posix_symlink_end_to_end() {
            let dir = tempdir().unwrap();
            let target_dir = dir.path().join("usr/share/zoneinfo/posix/America");
            fs::create_dir_all(&target_dir).unwrap();
            let target = target_dir.join("Denver");
            fs::write(&target, "fake tzdata").unwrap();

            let link = dir.path().join("localtime");
            symlink(&target, &link).unwrap();

            assert_eq!(
                detect_from_etc_localtime(&link),
                Some("America/Denver".to_string())
            );
        }

        #[test]
        fn two_hop_relative_chain_is_followed() {
            let dir = tempdir().unwrap();
            let target_dir = dir.path().join("usr/share/zoneinfo/America");
            fs::create_dir_all(&target_dir).unwrap();
            let target = target_dir.join("Denver");
            fs::write(&target, "fake tzdata").unwrap();

            // localtime -> tz-current (relative) -> .../zoneinfo/America/Denver
            let middle = dir.path().join("tz-current");
            symlink(&target, &middle).unwrap();
            let link = dir.path().join("localtime");
            symlink("tz-current", &link).unwrap();

            assert_eq!(
                detect_from_etc_localtime(&link),
                Some("America/Denver".to_string())
            );
        }

        #[test]
        fn relative_hop_resolves_against_its_own_links_directory() {
            let dir = tempdir().unwrap();
            let target_dir = dir.path().join("usr/share/zoneinfo/Asia");
            fs::create_dir_all(&target_dir).unwrap();
            fs::write(target_dir.join("Tokyo"), "fake tzdata").unwrap();

            // localtime -> var/lib/tz/a (absolute) -> b (relative to var/lib/tz)
            //   -> ../../../usr/share/zoneinfo/Asia/Tokyo (relative to var/lib/tz)
            let lib = dir.path().join("var/lib/tz");
            fs::create_dir_all(&lib).unwrap();
            symlink("../../../usr/share/zoneinfo/Asia/Tokyo", lib.join("b")).unwrap();
            symlink("b", lib.join("a")).unwrap();
            let link = dir.path().join("localtime");
            symlink(lib.join("a"), &link).unwrap();

            assert_eq!(
                detect_from_etc_localtime(&link),
                Some("Asia/Tokyo".to_string())
            );
        }

        #[test]
        fn three_hop_absolute_chain_is_followed() {
            let dir = tempdir().unwrap();
            let target_dir = dir.path().join("usr/share/zoneinfo/Europe");
            fs::create_dir_all(&target_dir).unwrap();
            let target = target_dir.join("London");
            fs::write(&target, "fake tzdata").unwrap();

            let lib = dir.path().join("var/lib");
            fs::create_dir_all(&lib).unwrap();
            let second = lib.join("tzlink2");
            symlink(&target, &second).unwrap();
            let first = lib.join("tzlink1");
            symlink(&second, &first).unwrap();
            let link = dir.path().join("localtime");
            symlink(&first, &link).unwrap();

            assert_eq!(
                detect_from_etc_localtime(&link),
                Some("Europe/London".to_string())
            );
        }

        #[test]
        fn first_zoneinfo_hop_wins_so_aliases_are_kept() {
            let dir = tempdir().unwrap();
            let zoneinfo = dir.path().join("usr/share/zoneinfo");
            fs::create_dir_all(zoneinfo.join("America")).unwrap();
            fs::create_dir_all(zoneinfo.join("US")).unwrap();
            fs::write(zoneinfo.join("America/Denver"), "fake tzdata").unwrap();
            symlink("../America/Denver", zoneinfo.join("US/Mountain")).unwrap();

            let link = dir.path().join("localtime");
            symlink(zoneinfo.join("US/Mountain"), &link).unwrap();

            assert_eq!(
                detect_from_etc_localtime(&link),
                Some("US/Mountain".to_string())
            );
        }

        #[test]
        fn symlink_loop_returns_none() {
            let dir = tempdir().unwrap();
            let a = dir.path().join("a");
            let b = dir.path().join("b");
            symlink(&b, &a).unwrap();
            symlink(&a, &b).unwrap();
            assert_eq!(detect_from_etc_localtime(&a), None);
        }

        #[test]
        fn non_symlink_returns_none() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("localtime");
            fs::write(&path, "not a symlink").unwrap();
            assert_eq!(detect_from_etc_localtime(&path), None);
        }

        #[test]
        fn missing_path_returns_none() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("does-not-exist");
            assert_eq!(detect_from_etc_localtime(&path), None);
        }
    }

    mod timedatectl_tests {
        use super::*;

        #[test]
        fn successful_output_is_used() {
            assert_eq!(
                parse_timedatectl_output(true, b"Asia/Tokyo\n"),
                Some("Asia/Tokyo".to_string())
            );
        }

        #[test]
        fn failed_command_returns_none() {
            assert_eq!(parse_timedatectl_output(false, b"Asia/Tokyo\n"), None);
        }

        #[test]
        fn empty_output_returns_none() {
            assert_eq!(parse_timedatectl_output(true, b""), None);
        }

        #[test]
        fn invalid_output_returns_none() {
            assert_eq!(parse_timedatectl_output(true, b"not@valid"), None);
        }
    }
}
