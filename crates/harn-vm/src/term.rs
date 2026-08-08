const DEFAULT_TERM_WIDTH: usize = 80;
const DEFAULT_TERM_HEIGHT: usize = 24;

pub(crate) fn width() -> usize {
    read_dimension("COLUMNS", true)
}

pub(crate) fn height() -> usize {
    read_dimension("LINES", false)
}

fn read_dimension(env_var: &str, is_width: bool) -> usize {
    if let Some(value) = dimension_from_env(std::env::var(env_var).ok().as_deref()) {
        return value;
    }
    platform_dimensions()
        .map(|(w, h)| if is_width { w } else { h })
        .unwrap_or(if is_width {
            DEFAULT_TERM_WIDTH
        } else {
            DEFAULT_TERM_HEIGHT
        })
}

pub(crate) fn dimension_from_env(raw: Option<&str>) -> Option<usize> {
    raw?.trim().parse::<usize>().ok().filter(|value| *value > 0)
}

#[cfg(unix)]
fn platform_dimensions() -> Option<(usize, usize)> {
    let mut winsize = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: `winsize` points to valid writable storage and `TIOCGWINSZ`
    // initializes it without retaining the pointer.
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: a zero return from `ioctl(TIOCGWINSZ)` means the kernel wrote
    // the `winsize` structure.
    let winsize = unsafe { winsize.assume_init() };
    if winsize.ws_col == 0 || winsize.ws_row == 0 {
        return None;
    }
    Some((winsize.ws_col as usize, winsize.ws_row as usize))
}

#[cfg(not(unix))]
fn platform_dimensions() -> Option<(usize, usize)> {
    None
}

#[cfg(test)]
mod tests {
    use super::dimension_from_env;

    #[test]
    fn dimensions_ignore_invalid_env_values() {
        assert_eq!(dimension_from_env(Some("132")), Some(132));
        assert_eq!(dimension_from_env(Some(" 48 ")), Some(48));
        assert_eq!(dimension_from_env(Some("0")), None);
        assert_eq!(dimension_from_env(Some("wide")), None);
        assert_eq!(dimension_from_env(None), None);
    }
}
