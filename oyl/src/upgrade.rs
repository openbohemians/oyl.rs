//! A YAML 1.1 file made YAML 1.2 in place: [`upgrade`].

use crate::{bytes, c_string, resume_panic, sys, Dialect, Error, ErrorKind, Mark, Parser, State};

/// What [`upgrade`] reads the file as, and its policy. Values keep the
/// reader's meaning and keys their text (as YAML 1.2 reads them) unless
/// set otherwise.
///
/// ```
/// use oyl::{Dialect, Upgrade};
///
/// let pyyaml = Upgrade::from(Dialect::yaml11());    // Ansible's files
/// let unknown = Upgrade::default();                 // the reader unknown
/// let keys_too = Upgrade::from(Dialect::goyaml2()).keys_keep_value(true);
/// # let _ = (pyyaml, unknown, keys_too);
/// ```
#[derive(Clone, Copy, Default)]
pub struct Upgrade<'a> {
    reader: Option<Dialect<'a>>,
    flags: u32,
}

impl<'a> From<Dialect<'a>> for Upgrade<'a> {
    /// The file's reader: [`Dialect::yaml11`] for PyYAML,
    /// [`Dialect::goyaml2`] for go-yaml v2, or a program's.
    fn from(reader: Dialect<'a>) -> Self {
        Upgrade { reader: Some(reader), flags: 0 }
    }
}

impl Upgrade<'_> {
    fn flag(mut self, flag: u32, on: bool) -> Self {
        if on { self.flags |= flag } else { self.flags &= !flag }
        self
    }

    /// Keys keep the reader's value too (`on:` becomes `true:`).
    pub fn keys_keep_value(self, on: bool) -> Self {
        self.flag(sys::OYL_UPGRADE_KEYS_KEEP_VALUE, on)
    }

    /// Values keep YAML 1.2's reading too (`yes` becomes `"yes"`).
    pub fn values_keep_text(self, on: bool) -> Self {
        self.flag(sys::OYL_UPGRADE_VALUES_KEEP_TEXT, on)
    }

    /// A timestamp's value is its text, as go-yaml v2 hands it to programs
    /// (implied by [`Dialect::goyaml2`]).
    pub fn timestamps_text(self, on: bool) -> Self {
        self.flag(sys::OYL_UPGRADE_TIMESTAMPS_TEXT, on)
    }
}

/// What a [`Fix`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FixKind {
    /// A scalar YAML 1.2 reads otherwise.
    Scalar,
    /// `%YAML 1.1`, removed.
    Directive,
    /// `\'` in double quotes (go-yaml v2's), the backslash removed.
    Escape,
    /// A block scalar ending the input without a line break.
    Chomp,
    /// A key go-yaml v2's merge order overrides, commented out.
    Merge,
    /// A quoted scalar's or flow collection's line not indented past its
    /// block, indented.
    Indent,
}

/// A change [`upgrade`] made, or a difference it found and left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// What it is.
    pub kind: FixKind,
    /// Where, in the input.
    pub mark: Mark,
    /// The input's text there.
    pub before: String,
    /// What replaced it (empty if removed, or left).
    pub after: String,
    /// False when only reported, the input left as it was.
    pub fixed: bool,
    /// What each reader reads, and what was done.
    pub message: String,
}

/// Makes `input`, a YAML 1.1 file, read the same in YAML 1.2, rewriting
/// only what a 1.2 reader would read otherwise and keeping every other
/// byte: comments, layout, quoting, key order. Returns the new text and
/// each change, with what couldn't be fixed (reported, left as it was).
///
/// With a reader named, afterwards the file reads the same in it and in
/// YAML 1.2. With [`Upgrade::default`], the reader is unknown: what PyYAML
/// and go-yaml v2 read alike is fixed, what they read otherwise reported.
///
/// ```
/// use oyl::{Dialect, Upgrade};
///
/// let (text, fixes) = oyl::upgrade("on: push\nenabled: yes\nmode: 0755\n",
///                                  &Upgrade::from(Dialect::yaml11())).unwrap();
/// assert_eq!(text, "\"on\": push\nenabled: true\nmode: !!int 0o755\n");
/// assert!(fixes.iter().all(|f| f.fixed));
/// ```
///
/// An error for input the reader can't read, or if the output would read
/// otherwise than decided (a bug, never a file).
pub fn upgrade(input: &str, options: &Upgrade<'_>) -> Result<(String, Vec<Fix>), Error> {
    let mut parser = Parser::new(input)?;
    if let Some(d) = options.reader {
        parser.dialect(d);
    }
    let mut out = sys::oyl_str { data: std::ptr::null(), len: 0 };
    let mut fixes: *const sys::oyl_fix = std::ptr::null();
    let mut n = 0usize;
    let status = unsafe { sys::oyl_upgrade(parser.parser.as_ptr(), options.flags, &mut out, &mut fixes, &mut n) };
    resume_panic();
    parser.state.set(State::Done);
    if status != sys::OYL_OK {
        return Err(parser.error(status));
    }
    // SAFETY: the output and the fixes are in the parser's arena, alive
    // until the parser drops at the end of this function: copy them out
    let text = String::from_utf8(unsafe { bytes(out) }.to_vec()).map_err(|_| Error {
        kind: ErrorKind::Other,
        message: "upgraded YAML is not UTF-8".into(),
        mark: None,
    })?;
    let raw: &[sys::oyl_fix] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(fixes, n) } };
    let list = raw
        .iter()
        .map(|f| Fix {
            kind: match f.kind {
                sys::OYL_FIX_DIRECTIVE => FixKind::Directive,
                sys::OYL_FIX_ESCAPE => FixKind::Escape,
                sys::OYL_FIX_CHOMP => FixKind::Chomp,
                sys::OYL_FIX_MERGE => FixKind::Merge,
                sys::OYL_FIX_INDENT => FixKind::Indent,
                _ => FixKind::Scalar,
            },
            mark: Mark::from(f.mark),
            before: String::from_utf8_lossy(unsafe { bytes(f.before) }).into_owned(),
            after: String::from_utf8_lossy(unsafe { bytes(f.after) }).into_owned(),
            fixed: f.fixed,
            message: unsafe { c_string(f.message) }.unwrap_or_default(),
        })
        .collect();
    Ok((text, list))
}
