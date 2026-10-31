// Schemas and dialects a program builds (oyl_schema_builder, oyl_dialect_new).

use std::ffi::{c_char, c_int, c_void, CString};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

use oyl_sys as sys;

use crate::{bytes, guard, raw_str, Dialect, Error, ErrorKind, MergeOrder, Schema, Value, CALLBACKS};

// ── Flags ───────────────────────────────────────────────────

macro_rules! flags {
    ($(#[$doc:meta])* $name:ident { $($(#[$fdoc:meta])* $flag:ident = $value:expr;)* }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(u32);

        impl $name {
            /// None of them.
            pub const NONE: Self = $name(0);
            $($(#[$fdoc])* pub const $flag: Self = $name($value);)*

            /// The flags as Oyl has them.
            pub const fn bits(self) -> u32 {
                self.0
            }

            /// Whether every flag of `other` is set.
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;

            fn bitor(self, other: Self) -> Self {
                $name(self.0 | other.0)
            }
        }

        impl std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, other: Self) {
                self.0 |= other.0;
            }
        }
    };
}

flags! {
    /// Options for the built-in int parser ([`SchemaBuilder::ints`]).
    /// Decimal digits are always accepted.
    IntFlags {
        /// `+` or `-` before decimal digits.
        SIGN = sys::OYL_INT_SIGN;
        /// `0x1F`.
        HEX = sys::OYL_INT_HEX;
        /// `0o17`.
        OCT = sys::OYL_INT_OCT;
        /// `017`: a leading zero means octal.
        OCT_ZERO = sys::OYL_INT_OCT_ZERO;
        /// `0b101`.
        BIN = sys::OYL_INT_BIN;
        /// `0X`, `0O` and `0B` as well.
        PREFIX_CASE = sys::OYL_INT_PREFIX_CASE;
        /// A sign before a prefix: `-0x1F`.
        SIGN_PREFIX = sys::OYL_INT_SIGN_PREFIX;
        /// Underscores anywhere but first are ignored: `1_000`.
        UNDERSCORE = sys::OYL_INT_UNDERSCORE;
        /// Decline a value beyond 64 bits (else it is text).
        RANGE = sys::OYL_INT_RANGE;
        /// Underscores after the first digit or a prefix: `1_000`, `0x_1F`
        /// (YAML 1.1).
        DIGIT_UNDERSCORE = sys::OYL_INT_DIGIT_UNDERSCORE;
        /// Base 60: `1:20` is 80 (YAML 1.1); the sign, underscore and range
        /// options apply.
        BASE60 = sys::OYL_INT_BASE60;
        /// A sign after a lowercase `0b` with none before it: `0b-101` is -5
        /// (go-yaml v2).
        BIN_SIGN = sys::OYL_INT_BIN_SIGN;
        /// The Core schema's ints, with a sign also allowed before `0x` or
        /// `0o`.
        CORE = sys::OYL_INT_CORE;
        /// YAML 1.1's ints.
        YAML11 = sys::OYL_INT_YAML11;
    }
}

flags! {
    /// Options for the built-in float parser ([`SchemaBuilder::floats`]),
    /// which always accepts decimal digits with a dot or an exponent
    /// (`1.5`, `.5`, `1e3`), signed or not.
    FloatFlags {
        /// `.inf`, `.Inf`, `.INF`, signed or not; `.nan`, `.NaN`, `.NAN`.
        SPECIALS = sys::OYL_FLOAT_SPECIALS;
        /// Digits alone, as when an int is too big.
        DIGITS = sys::OYL_FLOAT_DIGITS;
        /// Underscores, as go-yaml v2 takes them: anywhere after a leading
        /// digit or sign, else only between digits.
        UNDERSCORE = sys::OYL_FLOAT_UNDERSCORE;
        /// Decline a value too large for an `f64`.
        RANGE = sys::OYL_FLOAT_RANGE;
        /// A dot is required: `1e3` is not a float (YAML 1.1).
        NEED_DOT = sys::OYL_FLOAT_NEED_DOT;
        /// An exponent needs a sign: `1.0e+3` (YAML 1.1).
        EXP_SIGN = sys::OYL_FLOAT_EXP_SIGN;
        /// Underscores after the first digit and in the fraction, not the
        /// exponent (YAML 1.1).
        DIGIT_UNDERSCORE = sys::OYL_FLOAT_DIGIT_UNDERSCORE;
        /// Base 60: `1:20.5` is 80.5 (YAML 1.1).
        BASE60 = sys::OYL_FLOAT_BASE60;
        /// No sign before a leading dot: `.5` but not `-.5` (PyYAML).
        DOT_NO_SIGN = sys::OYL_FLOAT_DOT_NO_SIGN;
        /// The Core schema's floats. Digits alone count for an explicit
        /// `!!float`; untagged, an int rule before it takes them first.
        CORE = sys::OYL_FLOAT_CORE;
        /// YAML 1.1's floats, as PyYAML reads them.
        YAML11 = sys::OYL_FLOAT_YAML11;
    }
}

flags! {
    /// Syntax leniencies a [`CustomDialect`] may take on: each is an
    /// error, or reads differently, in YAML 1.2.
    Syntax {
        /// `\'` in double quotes is `'` (go-yaml v2).
        SQUOTE_ESCAPE = sys::OYL_SYNTAX_SQUOTE_ESCAPE;
        /// A block scalar ending the input gets no final line feed
        /// (libyaml, so PyYAML and go-yaml v2).
        EOF_NO_BREAK = sys::OYL_SYNTAX_EOF_NO_BREAK;
        /// A `%YAML` other than 1.1 is an error, in the parser's dialect
        /// for every document but one choosing a dialect given for its
        /// version or `%TAG !!` prefix (go-yaml v2).
        YAML11_ONLY = sys::OYL_SYNTAX_YAML11_ONLY;
        /// A quoted scalar's or flow collection's later lines needn't be
        /// indented past the block it sits in, and a tab may indent them:
        /// `[a,\nb]` (libyaml, so PyYAML and go-yaml v2).
        FLOW_INDENT = sys::OYL_SYNTAX_FLOW_INDENT;
        /// go-yaml v2's leniencies.
        GOYAML2 = sys::OYL_SYNTAX_GOYAML2;
    }
}

impl Syntax {
    pub(crate) fn from_bits(bits: u32) -> Self {
        Syntax(bits)
    }
}

// ── Schema builder ──────────────────────────────────────────

/// A custom type's parser: the value of a scalar's text, or `None` to
/// decline it. Text values borrow from the text (or are `'static`).
type ParseFn = dyn for<'t> Fn(&'t [u8]) -> Option<Value<'t>> + Send + Sync;

enum Rule {
    Word { word: String, tag: String, ignore_case: bool },
    Bools { yes: Vec<String>, no: Vec<String> },
    Nulls(Vec<String>),
    Ints(IntFlags),
    Floats(FloatFlags),
    Type { tag: String, parse: Box<ParseFn> },
}

/// Builds a [`CustomSchema`]: a list of rules, each binding a tag to the
/// scalars it accepts. A plain scalar takes the tag of the first rule that
/// accepts it, and that rule gives its value; a scalar no rule accepts is a
/// string (`tag:yaml.org,2002:str`). A scalar with an explicit tag is read
/// by that tag's rules, and a tag with no rules gives the text.
///
/// ```
/// use oyl::{FloatFlags, IntFlags, Parser, SchemaBuilder, Value};
///
/// // YAML 1.2's JSON types, plus `yes` and `no` as booleans
/// let schema = SchemaBuilder::new()
///     .nulls(&["null"])
///     .bools(&["true", "yes"], &["false", "no"])
///     .ints(IntFlags::SIGN)
///     .floats(FloatFlags::NONE)
///     .build()?;
/// let mut parser = Parser::new("on: yes\nport: 8080\nname: 0x10\n")?;
/// parser.schema(schema.schema());
/// let tree = parser.next_tree()?.unwrap();
/// let root = tree.root();
/// assert_eq!(root.get("on").unwrap().value()?, Value::Bool(true));
/// assert_eq!(root.get("port").unwrap().value()?, Value::Int(8080));
/// assert_eq!(root.get("name").unwrap().value()?, Value::Str(b"0x10"));
/// # Ok::<(), oyl::Error>(())
/// ```
#[derive(Default)]
pub struct SchemaBuilder {
    rules: Vec<Rule>,
}

impl SchemaBuilder {
    /// A builder with no rules: everything is a string until rules are
    /// added.
    pub fn new() -> Self {
        SchemaBuilder::default()
    }

    /// Words that are null (`tag:yaml.org,2002:null`). `""` makes an empty
    /// scalar null, as in YAML 1.2.
    pub fn nulls(mut self, words: &[&str]) -> Self {
        self.rules.push(Rule::Nulls(words.iter().map(|&w| w.to_owned()).collect()));
        self
    }

    /// Words that are true and false (`tag:yaml.org,2002:bool`).
    pub fn bools(mut self, true_words: &[&str], false_words: &[&str]) -> Self {
        let owned = |words: &[&str]| words.iter().map(|&w| w.to_owned()).collect();
        self.rules.push(Rule::Bools { yes: owned(true_words), no: owned(false_words) });
        self
    }

    /// The built-in int parser (`tag:yaml.org,2002:int`), with these
    /// options.
    pub fn ints(mut self, flags: IntFlags) -> Self {
        self.rules.push(Rule::Ints(flags));
        self
    }

    /// The built-in float parser (`tag:yaml.org,2002:float`), with these
    /// options.
    pub fn floats(mut self, flags: FloatFlags) -> Self {
        self.rules.push(Rule::Floats(flags));
        self
    }

    /// A word with `tag`, a full tag (`tag:example.com,2000:color`) or a
    /// local one (`!color`). Its value is its text; but with the null tag
    /// it is null, and with the bool tag false for `false`, `no`, `n` and
    /// `off` in any case and true otherwise (for other words,
    /// [`bools`](SchemaBuilder::bools)).
    pub fn word(mut self, word: &str, tag: &str) -> Self {
        self.rules.push(Rule::Word { word: word.to_owned(), tag: tag.to_owned(), ignore_case: false });
        self
    }

    /// As [`word`](SchemaBuilder::word), ignoring ASCII case.
    pub fn word_ignore_case(mut self, word: &str, tag: &str) -> Self {
        self.rules.push(Rule::Word { word: word.to_owned(), tag: tag.to_owned(), ignore_case: true });
        self
    }

    /// A type of your own: `parse` gives the value of a scalar's text, or
    /// `None` to decline it. Untagged scalars it accepts get `tag`, and
    /// scalars tagged `tag` are read by it. Binding a built-in tag
    /// (`tag:yaml.org,2002:int`) redefines it. A text value must borrow
    /// from the text it is given (or be `'static`).
    ///
    /// `parse` may be called from any thread that parses with the schema.
    /// A panic in it goes on from the call that was parsing.
    ///
    /// ```
    /// use oyl::{Parser, SchemaBuilder, Value};
    ///
    /// // sizes such as 4Ki, read as ints
    /// let schema = SchemaBuilder::new()
    ///     .custom("tag:example.com,2000:size", |text: &[u8]| {
    ///         let digits = text.strip_suffix(b"Ki")?;
    ///         let n: i64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    ///         Some(Value::Int(n * 1024))
    ///     })
    ///     .build()?;
    /// let mut parser = Parser::new("memory: 4Ki\nname: web\n")?;
    /// parser.schema(schema.schema());
    /// let tree = parser.next_tree()?.unwrap();
    /// let memory = tree.root().get("memory").unwrap();
    /// assert_eq!(memory.value()?, Value::Int(4096));
    /// assert_eq!(memory.tag(), Some(&b"tag:example.com,2000:size"[..]));
    /// assert_eq!(tree.root().get("name").unwrap().value()?, Value::Str(b"web"));
    /// # Ok::<(), oyl::Error>(())
    /// ```
    pub fn custom<F>(mut self, tag: &str, parse: F) -> Self
    where
        F: for<'t> Fn(&'t [u8]) -> Option<Value<'t>> + Send + Sync + 'static,
    {
        self.rules.push(Rule::Type { tag: tag.to_owned(), parse: Box::new(parse) });
        self
    }

    /// The schema. An [`ErrorKind::Input`] error if a word holds a NUL
    /// byte.
    pub fn build(self) -> Result<CustomSchema, Error> {
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(4096) }).ok_or_else(Error::memory)?;
        // owns the arena and the parsers from here, freeing them on failure
        let mut out = CustomSchema { arena, schema: NonNull::dangling(), parsers: Vec::new() };
        let b = NonNull::new(unsafe { sys::oyl_schema_builder_new(arena.as_ptr()) }).ok_or_else(Error::memory)?;
        // finish copies the words and tags into the arena: they live until then
        let mut keep = Keep::default();
        let added = add_rules(self.rules, b.as_ptr(), &mut keep, &mut out.parsers);
        let schema = added.and_then(|()| {
            NonNull::new(unsafe { sys::oyl_schema_builder_finish(b.as_ptr()) }.cast_mut()).ok_or_else(Error::memory)
        });
        unsafe { sys::oyl_schema_builder_free(b.as_ptr()) };
        drop(keep);
        out.schema = schema?;
        Ok(out)
    }
}

/// What the builder points to until `finish`: words as C strings, and tags.
#[derive(Default)]
struct Keep {
    words: Vec<CString>,
    tags: Vec<String>,
}

impl Keep {
    /// `words` as C strings, kept; the pointers are for C.
    fn words(&mut self, words: Vec<String>) -> Result<Vec<*const c_char>, Error> {
        let mut ptrs = Vec::with_capacity(words.len());
        for w in words {
            let c = c_word(&w)?;
            ptrs.push(c.as_ptr());
            self.words.push(c);
        }
        Ok(ptrs)
    }

    /// `tag` for C, kept (its bytes stay where they are when it moves).
    fn tag(&mut self, tag: String) -> sys::oyl_str {
        let raw = raw_str(Some(tag.as_bytes()));
        self.tags.push(tag);
        raw
    }
}

/// Give the rules to `b`. Custom types' parsers move to `parsers`, which
/// the schema frees.
fn add_rules(
    rules: Vec<Rule>,
    b: *mut sys::oyl_schema_builder,
    keep: &mut Keep,
    parsers: &mut Vec<NonNull<Box<ParseFn>>>,
) -> Result<(), Error> {
    for rule in rules {
        match rule {
            Rule::Word { word, tag, ignore_case } => {
                let pattern = keep.words(vec![word])?;
                let how = if ignore_case { sys::OYL_MATCH_ICASE } else { sys::OYL_MATCH_EXACT };
                let tag = keep.tag(tag);
                unsafe { sys::oyl_schema_builder_add(b, how, pattern[0], tag) };
            }
            Rule::Bools { yes, no } => {
                let (mut yes, mut no) = (keep.words(yes)?, keep.words(no)?);
                let (nyes, nno) = (count(&yes)?, count(&no)?);
                unsafe { sys::oyl_schema_builder_add_bools(b, yes.as_mut_ptr(), nyes, no.as_mut_ptr(), nno) };
            }
            Rule::Nulls(words) => {
                let mut words = keep.words(words)?;
                let n = count(&words)?;
                unsafe { sys::oyl_schema_builder_add_nulls(b, words.as_mut_ptr(), n) };
            }
            Rule::Ints(flags) => unsafe { sys::oyl_schema_builder_add_int_flags(b, flags.bits()) },
            Rule::Floats(flags) => unsafe { sys::oyl_schema_builder_add_float_flags(b, flags.bits()) },
            Rule::Type { tag, parse } => {
                // a thin pointer for C to the boxed closure
                let user = NonNull::from(Box::leak(Box::new(parse)));
                parsers.push(user);
                CALLBACKS.store(true, Ordering::Relaxed);
                let tag = keep.tag(tag);
                unsafe { sys::oyl_schema_builder_add_type(b, tag, Some(parse_custom), user.as_ptr().cast()) };
            }
        }
    }
    Ok(())
}

impl fmt::Debug for SchemaBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchemaBuilder").field("rules", &self.rules.len()).finish()
    }
}

fn c_word(word: &str) -> Result<CString, Error> {
    CString::new(word).map_err(|_| Error {
        kind: ErrorKind::Input,
        message: format!("the word {word:?} holds a NUL byte"),
        mark: None,
    })
}

fn count<T>(v: &[T]) -> Result<c_int, Error> {
    c_int::try_from(v.len()).map_err(|_| Error::from_status(sys::OYL_ERR_INPUT))
}

/// Oyl calls this for a custom type, with the schema's copy of its parser.
unsafe extern "C" fn parse_custom(text: sys::oyl_str, out: *mut sys::oyl_value, user: *mut c_void) -> bool {
    // SAFETY: `user` is a parser the schema owns, and the text is the
    // scalar's, readable for this call
    let parse = &*user.cast::<Box<ParseFn>>();
    let text = bytes(text);
    guard(false, || match parse(text) {
        Some(value) => {
            *out = value.raw();
            true
        }
        None => false,
    })
}

// ── Custom schemas ──────────────────────────────────────────

/// A schema a program built with a [`SchemaBuilder`]. Give its
/// [`schema`](CustomSchema::schema) to a parser ([`Parser::schema`]) or a
/// [`CustomDialect`]; they, and the trees read with them, can't outlive it:
///
/// ```compile_fail
/// let schema = oyl::SchemaBuilder::new().build().unwrap();
/// let mut parser = oyl::Parser::new("a").unwrap();
/// parser.schema(schema.schema());
/// drop(schema);
/// parser.next_tree().unwrap();
/// ```
///
/// [`Parser::schema`]: crate::Parser::schema
pub struct CustomSchema {
    arena: NonNull<sys::oyl_arena>,
    schema: NonNull<sys::oyl_schema>,
    parsers: Vec<NonNull<Box<ParseFn>>>,
}

// SAFETY: a built schema is never changed, and its parsers are Send + Sync
unsafe impl Send for CustomSchema {}
unsafe impl Sync for CustomSchema {}

impl CustomSchema {
    /// The schema, borrowing from this.
    pub fn schema(&self) -> Schema<'_> {
        Schema(self.schema, PhantomData)
    }
}

impl Drop for CustomSchema {
    fn drop(&mut self) {
        unsafe { sys::oyl_arena_free(self.arena.as_ptr()) };
        for parse in self.parsers.drain(..) {
            // SAFETY: each was leaked from a box in add_rules
            drop(unsafe { Box::from_raw(parse.as_ptr()) });
        }
    }
}

impl fmt::Debug for CustomSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CustomSchema")
    }
}

// ── Custom dialects ─────────────────────────────────────────

/// A dialect a program builds: a schema, syntax leniencies and a merge
/// order, of one YAML version. Give its [`dialect`](CustomDialect::dialect)
/// to a parser ([`Parser::dialect`], [`Parser::dialect_for_version`],
/// [`Parser::dialect_for_tag_prefix`]); the parser, and the trees read with
/// it, can't outlive it.
///
/// ```
/// use oyl::{CustomDialect, MergeOrder, Parser, Schema, Syntax};
///
/// // YAML 1.2, with << an ordinary key
/// let dialect = CustomDialect::new((1, 2), Schema::core(), Syntax::NONE, MergeOrder::None)?;
/// let mut parser = Parser::new("base: &b {x: 1}\nm: {<<: *b}\n")?;
/// parser.dialect(dialect.dialect());
/// let tree = parser.next_tree()?.unwrap();
/// let m = tree.root().get("m").unwrap();
/// assert!(m.get("x").is_none() && m.get("<<").is_some());
/// # Ok::<(), oyl::Error>(())
/// ```
///
/// A tree read with it can't outlive it:
///
/// ```compile_fail
/// use oyl::{CustomDialect, MergeOrder, Parser, Schema, Syntax};
///
/// let d = CustomDialect::new((1, 2), Schema::core(), Syntax::NONE, MergeOrder::Yaml).unwrap();
/// let tree = {
///     let mut p = Parser::new("a").unwrap();
///     p.dialect(d.dialect());
///     p.next_tree().unwrap().unwrap()
/// };
/// drop(d);
/// tree.root();
/// ```
///
/// [`Parser::dialect`]: crate::Parser::dialect
/// [`Parser::dialect_for_version`]: crate::Parser::dialect_for_version
/// [`Parser::dialect_for_tag_prefix`]: crate::Parser::dialect_for_tag_prefix
pub struct CustomDialect<'s> {
    arena: NonNull<sys::oyl_arena>,
    dialect: NonNull<sys::oyl_dialect>,
    _schema: PhantomData<Schema<'s>>,
}

// SAFETY: a dialect is never changed, and its schema is Send + Sync
unsafe impl Send for CustomDialect<'_> {}
unsafe impl Sync for CustomDialect<'_> {}

impl<'s> CustomDialect<'s> {
    /// A dialect of YAML `version` (`(1, 1)`, `(1, 2)`). An
    /// [`ErrorKind::Input`] error for a version not 1.x.
    pub fn new(version: (u32, u32), schema: Schema<'s>, syntax: Syntax, merge: MergeOrder) -> Result<Self, Error> {
        let (major, minor) = version;
        if major != 1 || c_int::try_from(minor).is_err() {
            return Err(Error {
                kind: ErrorKind::Input,
                message: format!("{major}.{minor} is not a YAML 1.x version"),
                mark: None,
            });
        }
        let v = CString::new(format!("{major}.{minor}")).expect("digits and a dot");
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(256) }).ok_or_else(Error::memory)?;
        let d = unsafe { sys::oyl_dialect_new(arena.as_ptr(), v.as_ptr(), schema.0.as_ptr(), syntax.bits(), merge.raw()) };
        match NonNull::new(d.cast_mut()) {
            Some(dialect) => Ok(CustomDialect { arena, dialect, _schema: PhantomData }),
            None => {
                unsafe { sys::oyl_arena_free(arena.as_ptr()) };
                Err(Error::memory())
            }
        }
    }

    /// The dialect, borrowing from this.
    pub fn dialect(&self) -> Dialect<'_> {
        Dialect(self.dialect, PhantomData)
    }
}

impl Drop for CustomDialect<'_> {
    fn drop(&mut self) {
        unsafe { sys::oyl_arena_free(self.arena.as_ptr()) };
    }
}

impl fmt::Debug for CustomDialect<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.dialect();
        f.debug_struct("CustomDialect")
            .field("version", &d.version())
            .field("syntax", &d.syntax())
            .field("merge_order", &d.merge_order())
            .finish()
    }
}
