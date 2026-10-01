//! Archives on a server: compress files/folders (zip, tar.gz, tar.xz, tar),
//! extract archives, and stream a folder as an archive straight to the
//! browser. Runs the server's own `tar` / `zip` / `unzip` over SSH as the
//! account, so data never round-trips through the bastion (except streamed
//! downloads, which don't touch the server's disk).
//!
//! Every name is single-quoted and prefixed with `./`, so a file called
//! `-rf` or `x'; rm -rf ~` is just a file name.

use crate::error::{AppError, AppResult};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Zip,
    TarGz,
    TarXz,
    Tar,
}

impl Format {
    pub fn parse(s: &str) -> AppResult<Self> {
        Ok(match s {
            "zip" => Format::Zip,
            "tar.gz" | "tgz" => Format::TarGz,
            "tar.xz" | "txz" => Format::TarXz,
            "tar" => Format::Tar,
            other => return Err(AppError::BadRequest(format!("unsupported format `{other}` (zip, tar.gz, tar.xz, tar)"))),
        })
    }
    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::TarGz => ".tar.gz",
            Format::TarXz => ".tar.xz",
            Format::Tar => ".tar",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Format::Zip => "application/zip",
            Format::TarGz => "application/gzip",
            Format::TarXz => "application/x-xz",
            Format::Tar => "application/x-tar",
        }
    }
    fn tar_flag(self) -> &'static str {
        match self {
            Format::TarGz => "z",
            Format::TarXz => "J",
            _ => "",
        }
    }
}

/// Single-quote for sh.
pub fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A name inside the current folder: no slashes, not `.`/`..`, not empty.
pub fn check_name(n: &str) -> AppResult<()> {
    if n.is_empty() || n == "." || n == ".." || n.contains('/') || n.contains('\0') || n.len() > 255 {
        return Err(AppError::BadRequest(format!("invalid name `{n}`")));
    }
    Ok(())
}

fn names(list: &[String]) -> AppResult<String> {
    if list.is_empty() {
        return Err(AppError::BadRequest("choose files or folders".into()));
    }
    list.iter().map(|n| check_name(n).map(|_| q(&format!("./{n}")))).collect::<AppResult<Vec<_>>>().map(|v| v.join(" "))
}

/// Create `archive` (a name in `dir`) from `items` (names in `dir`).
pub fn compress_cmd(dir: &str, items: &[String], format: Format, archive: &str) -> AppResult<String> {
    check_name(archive)?;
    let files = names(items)?;
    let a = q(&format!("./{archive}"));
    let make = match format {
        Format::Zip => format!("zip -r -q -y {a} {files}"),
        f => format!("tar -c{}f {a} {files}", f.tar_flag()),
    };
    Ok(format!("cd -- {} && if [ -e {a} ]; then echo 'timika:exists' >&2; exit 9; fi && {make}", q(dir)))
}

/// Stream an archive of `items` (names in `dir`) to stdout.
pub fn stream_cmd(dir: &str, items: &[String], format: Format) -> AppResult<String> {
    let files = names(items)?;
    let make = match format {
        Format::Zip => format!("zip -r -q -y - {files}"),
        f => format!("tar -c{}f - {files}", f.tar_flag()),
    };
    Ok(format!("cd -- {} && {make}", q(dir)))
}

/// Which archive a file name is, and the folder name to extract it into.
pub fn archive_kind(name: &str) -> Option<(bool, String)> {
    let lower = name.to_lowercase();
    for ext in [".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tar", ".zip"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return Some((ext == ".zip", name[..name.len() - ext.len()].to_string()));
        }
    }
    None
}

/// Extract `archive` (a name in `dir`) into a new folder `into` (a name in `dir`).
pub fn extract_cmd(dir: &str, archive: &str, into: &str) -> AppResult<String> {
    check_name(archive)?;
    check_name(into)?;
    let (zip, _) = archive_kind(archive).ok_or_else(|| AppError::BadRequest(format!("`{archive}` isn't a zip or tar archive")))?;
    let (a, d) = (q(&format!("./{archive}")), q(&format!("./{into}")));
    // `tar -xf` detects gzip/xz/bzip2 itself; GNU tar and unzip refuse `..` and
    // absolute paths inside archives.
    let unpack = if zip { format!("unzip -q {a} -d {d}") } else { format!("tar -xf {a} -C {d}") };
    Ok(format!(
        "cd -- {} && if [ -e {d} ]; then echo 'timika:exists' >&2; exit 9; fi && mkdir -- {d} && {{ {unpack} || {{ rc=$?; rm -rf -- {d}; exit $rc; }}; }}",
        q(dir)
    ))
}

/// Turn a failed command's exit status and output into a message.
pub fn explain(status: u32, out: &str, what: &str) -> AppError {
    if out.contains("timika:exists") {
        return AppError::Conflict(format!("{what}: a file or folder with that name already exists"));
    }
    if status == 127 || out.contains("not found") && (out.contains("zip") || out.contains("tar")) {
        let tool = if out.contains("unzip") { "unzip" } else if out.contains("zip") { "zip" } else { "tar" };
        return AppError::BadRequest(format!("`{tool}` isn't installed on the server — install it, or choose a tar format"));
    }
    let tail: String = out.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").chars().take(200).collect();
    AppError::BadRequest(format!("{what} failed (exit {status}){}", if tail.is_empty() { String::new() } else { format!(": {tail}") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hostile_names_stay_names() {
        let c = compress_cmd("/srv/my dir", &v(&["-rf", "x'; rm -rf ~ #"]), Format::TarGz, "out.tar.gz").unwrap();
        assert!(c.starts_with("cd -- '/srv/my dir' && "));
        assert!(c.contains("tar -czf './out.tar.gz' './-rf' './x'\\''; rm -rf ~ #'"));
    }

    #[test]
    fn names_must_be_in_the_folder() {
        assert!(compress_cmd("/srv", &v(&["../etc"]), Format::Zip, "a.zip").is_err());
        assert!(compress_cmd("/srv", &v(&["a/b"]), Format::Zip, "a.zip").is_err());
        assert!(compress_cmd("/srv", &v(&["a"]), Format::Zip, "../a.zip").is_err());
        assert!(compress_cmd("/srv", &[], Format::Zip, "a.zip").is_err());
    }

    #[test]
    fn formats() {
        assert!(compress_cmd("/", &v(&["a"]), Format::Zip, "a.zip").unwrap().contains("zip -r -q -y './a.zip' './a'"));
        assert!(compress_cmd("/", &v(&["a"]), Format::TarXz, "a.tar.xz").unwrap().contains("tar -cJf"));
        assert!(compress_cmd("/", &v(&["a"]), Format::Tar, "a.tar").unwrap().contains("tar -cf"));
        assert!(stream_cmd("/", &v(&["a"]), Format::Zip).unwrap().ends_with("zip -r -q -y - './a'"));
        assert!(stream_cmd("/", &v(&["a"]), Format::TarGz).unwrap().ends_with("tar -czf - './a'"));
    }

    #[test]
    fn extraction() {
        assert_eq!(archive_kind("site.tar.gz"), Some((false, "site".into())));
        assert_eq!(archive_kind("Backup.ZIP"), Some((true, "Backup".into())));
        assert_eq!(archive_kind("notes.txt"), None);
        assert_eq!(archive_kind(".zip"), None);
        let c = extract_cmd("/srv", "site.tar.gz", "site").unwrap();
        assert!(c.contains("mkdir -- './site'") && c.contains("tar -xf './site.tar.gz' -C './site'"));
        assert!(extract_cmd("/srv", "a.zip", "a").unwrap().contains("unzip -q './a.zip' -d './a'"));
        assert!(extract_cmd("/srv", "notes.txt", "n").is_err());
    }
}
