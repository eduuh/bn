use std::ffi::OsStr;
use std::path::Path;

pub(crate) fn name_from_arg(arg: Option<&OsStr>) -> &'static str {
    match arg.and_then(|arg| Path::new(arg).file_stem()?.to_str()) {
        Some("workspan" | "wp") => "workspan",
        _ => "bn",
    }
}

pub(crate) fn name() -> &'static str {
    name_from_arg(std::env::args_os().next().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_entry_points_select_their_identity() {
        assert_eq!(
            name_from_arg(Some(OsStr::new("/tools/workspan"))),
            "workspan"
        );
        assert_eq!(name_from_arg(Some(OsStr::new("wp.exe"))), "workspan");
        assert_eq!(name_from_arg(Some(OsStr::new("/tools/bn"))), "bn");
        assert_eq!(name_from_arg(None), "bn");
    }
}
