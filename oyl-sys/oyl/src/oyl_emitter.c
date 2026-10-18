/*
 * oyl_emitter.c — YAML 1.2 event emitter
 *
 * Consumes parser events and produces YAML text in three styles:
 *   - BLOCK: standard indented YAML (default)
 *   - FLOW:  JSON-like inline with spaces
 *   - MINIMAL: compact flow, no optional whitespace
 */

#include "oyl_internal.h"
#include "oyl_chars.h"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* ── Context types ───────────────────────────────────────── */

typedef enum {
    EMIT_CTX_BLOCK_MAP,
    EMIT_CTX_BLOCK_SEQ,
    EMIT_CTX_FLOW_MAP,
    EMIT_CTX_FLOW_SEQ,
} emit_ctx_type;

typedef struct {
    emit_ctx_type type;
    int           indent;     /* absolute column for this level */
    int           count;      /* entries emitted so far */
    bool          expect_key; /* in mappings: true=key, false=value */
    bool          complex_key; /* block mapping: current key used "? " */
} emit_ctx;

/* ── Emitter state ───────────────────────────────────────── */

typedef struct {
    oyl_emit_style style;
    int            indent;   /* spaces per level, 1-10 */
} emit_opts;

struct oyl_emitter {
    emit_opts     opts;
    oyl_arena    *arena;

    /* output buffer (malloc'd) */
    char   *buf;
    size_t  len;
    size_t  cap;

    /* context stack */
    emit_ctx *stack;
    int       stack_len;
    int       stack_cap;
    int       flow_depth;   /* flow collections on the stack */

    /* state */
    bool doc_open;
    bool first_doc;
    bool wrote_block_key; /* just wrote a block map key, value next */
    bool after_seq_dash;  /* just wrote "- " for a block seq entry */
    bool key_was_alias;   /* the current mapping key is an alias */
    bool wrote_props;     /* the current node has an anchor or tag */
};

/* ── Buffer management ───────────────────────────────────── */

#define EMIT_INIT_CAP 4096

NOINLINE oyl_status buf_grow(oyl_emitter *e, size_t need) {
    if (need > SIZE_MAX - e->len) return OYL_ERR_MEMORY;
    size_t new_cap = e->cap * 2;
    if (new_cap < e->len + need) new_cap = e->len + need;
    char *nb = realloc(e->buf, new_cap);
    if (!nb) return OYL_ERR_MEMORY;
    e->buf = nb;
    e->cap = new_cap;
    return OYL_OK;
}

static inline oyl_status buf_ensure(oyl_emitter *e, size_t need) {
    return need <= e->cap - e->len ? OYL_OK : buf_grow(e, need);
}

static inline oyl_status buf_put(oyl_emitter *e, char c) {
    oyl_status st = buf_ensure(e, 1);
    if (st != OYL_OK) return st;
    e->buf[e->len++] = c;
    return OYL_OK;
}

static oyl_status buf_puts(oyl_emitter *e, const char *s, size_t slen) {
    oyl_status st = buf_ensure(e, slen);
    if (st != OYL_OK) return st;
    if (slen) memcpy(e->buf + e->len, s, slen);
    e->len += slen;
    return OYL_OK;
}

#define PUTS(e, lit) buf_puts(e, lit, sizeof(lit) - 1)

static oyl_status buf_indent(oyl_emitter *e, int spaces) {
    if (spaces <= 0) return OYL_OK;
    oyl_status st = buf_ensure(e, (size_t)spaces);
    if (st != OYL_OK) return st;
    memset(e->buf + e->len, ' ', (size_t)spaces);
    e->len += (size_t)spaces;
    return OYL_OK;
}

/* ── Context stack ───────────────────────────────────────── */

static oyl_status push_ctx(oyl_emitter *e, emit_ctx_type type, int indent) {
    if (e->stack_len >= e->stack_cap) {
        int new_cap = e->stack_cap * 2;
        if (new_cap < 8) new_cap = 8;
        emit_ctx *ns = realloc(e->stack, (size_t)new_cap * sizeof(emit_ctx));
        if (!ns) return OYL_ERR_MEMORY;
        e->stack = ns;
        e->stack_cap = new_cap;
    }
    e->stack[e->stack_len++] = (emit_ctx){
        .type = type,
        .indent = indent,
        .count = 0,
        .expect_key = (type == EMIT_CTX_BLOCK_MAP || type == EMIT_CTX_FLOW_MAP),
    };
    if (type == EMIT_CTX_FLOW_MAP || type == EMIT_CTX_FLOW_SEQ) e->flow_depth++;
    return OYL_OK;
}

static emit_ctx *top_ctx(oyl_emitter *e) {
    if (e->stack_len == 0) return NULL;
    return &e->stack[e->stack_len - 1];
}

static void pop_ctx(oyl_emitter *e) {
    if (e->stack_len == 0) return;
    emit_ctx_type type = e->stack[--e->stack_len].type;
    if (type == EMIT_CTX_FLOW_MAP || type == EMIT_CTX_FLOW_SEQ) e->flow_depth--;
}

static bool in_any_flow(oyl_emitter *e) {
    return e->flow_depth > 0;
}

/* ── Separator strings ───────────────────────────────────── */

static const char *entry_sep(oyl_emitter *e) {
    return (e->opts.style == OYL_EMIT_MINIMAL) ? "," : ", ";
}

static int entry_sep_len(oyl_emitter *e) {
    return (e->opts.style == OYL_EMIT_MINIMAL) ? 1 : 2;
}

static const char *kv_sep(oyl_emitter *e) {
    (void)e;
    return ": "; /* always space after : for safe round-tripping */
}

static int kv_sep_len(oyl_emitter *e) {
    (void)e;
    return 2;
}

/* ── Scalar quoting decision ─────────────────────────────── */

static bool is_yaml_keyword(const char *s, size_t len) {
    if (len == 0) return false;
    /* null variants */
    if (len == 4 && memcmp(s, "null", 4) == 0) return true;
    if (len == 4 && memcmp(s, "Null", 4) == 0) return true;
    if (len == 4 && memcmp(s, "NULL", 4) == 0) return true;
    if (len == 1 && s[0] == '~') return true;
    /* bool variants */
    if (len == 4 && memcmp(s, "true", 4) == 0) return true;
    if (len == 4 && memcmp(s, "True", 4) == 0) return true;
    if (len == 4 && memcmp(s, "TRUE", 4) == 0) return true;
    if (len == 5 && memcmp(s, "false", 5) == 0) return true;
    if (len == 5 && memcmp(s, "False", 5) == 0) return true;
    if (len == 5 && memcmp(s, "FALSE", 5) == 0) return true;
    /* special floats */
    if (len == 4 && memcmp(s, ".inf", 4) == 0) return true;
    if (len == 5 && memcmp(s, "+.inf", 5) == 0) return true;
    if (len == 5 && memcmp(s, "-.inf", 5) == 0) return true;
    if (len == 4 && memcmp(s, ".nan", 4) == 0) return true;
    if (len == 4 && memcmp(s, ".Inf", 4) == 0) return true;
    if (len == 4 && memcmp(s, ".NaN", 4) == 0) return true;
    if (len == 4 && memcmp(s, ".Nan", 4) == 0) return true;
    if (len == 4 && memcmp(s, ".INF", 4) == 0) return true;
    if (len == 4 && memcmp(s, ".NAN", 4) == 0) return true;
    return false;
}

/* Forward declarations for int/float matchers from oyl_schema.c */
/* We duplicate the logic here to avoid exposing internal functions */
static bool looks_like_number(const char *s, size_t len) {
    if (len == 0) return false;
    size_t i = 0;
    if (s[0] == '+' || s[0] == '-') { i = 1; if (i >= len) return false; }
    /* hex/octal */
    if (i + 1 < len && s[i] == '0' && (s[i+1] == 'x' || s[i+1] == 'o'))
        return true; /* close enough — if it starts 0x/0o, quote it */
    /* digits */
    bool all_digit = true;
    bool has_dot = false;
    bool has_e = false;
    for (size_t j = i; j < len; j++) {
        if (s[j] >= '0' && s[j] <= '9') continue;
        if (s[j] == '.' && !has_dot) { has_dot = true; continue; }
        if ((s[j] == 'e' || s[j] == 'E') && !has_e) { has_e = true; continue; }
        if ((s[j] == '+' || s[j] == '-') && j > 0 && (s[j-1] == 'e' || s[j-1] == 'E')) continue;
        all_digit = false;
        break;
    }
    return all_digit;
}

/* What a scalar's bytes contain. One pass over it (scalar_classes) answers
 * most of what choosing its style asks; the rest is about its ends. */
enum {
    SC_CTRL  = 1 << 0,   /* a control character other than tab and \n, or DEL */
    SC_BREAK = 1 << 1,   /* \n or \r */
    SC_LF    = 1 << 2,   /* \n */
    SC_COLON = 1 << 3,
    SC_HASH  = 1 << 4,
    SC_FLOW  = 1 << 5,   /* , [ ] { } */
};

#define C_ SC_CTRL
#define LF (SC_BREAK | SC_LF)
#define CR (SC_CTRL | SC_BREAK)
#define CO SC_COLON
#define HA SC_HASH
#define FL SC_FLOW
static const uint8_t scalar_class[256] = {
    /*       0   1   2   3   4   5   6   7   8   9   A   B   C   D   E   F */
    /* 0x */ C_, C_, C_, C_, C_, C_, C_, C_, C_, 0,  LF, C_, C_, CR, C_, C_,
    /* 1x */ C_, C_, C_, C_, C_, C_, C_, C_, C_, C_, C_, C_, C_, C_, C_, C_,
    /* 2x */ 0,  0,  0,  HA, 0,  0,  0,  0,  0,  0,  0,  0,  FL, 0,  0,  0,
    /* 3x */ 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  CO, 0,  0,  0,  0,  0,
    /* 4x */ 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    /* 5x */ 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  FL, 0,  FL, 0,  0,
    /* 6x */ 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    /* 7x */ 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  FL, 0,  FL, 0,  C_,
    /* 8x-Fx: 0 */
};
#undef C_
#undef LF
#undef CR
#undef CO
#undef HA
#undef FL

static unsigned scalar_classes(const char *s, size_t len) {
    unsigned f = 0;
    for (size_t i = 0; i < len; i++) f |= scalar_class[(uint8_t)s[i]];
    return f;
}

/* Could this text be misread if emitted plain? Only syntax is considered
 * here; values that would resolve to another type (numbers, true, null)
 * are handled by choose_style. `f` is scalar_classes(s, len). */
static bool needs_quoting(const char *s, size_t len, bool flow_ctx, unsigned f) {
    if (len == 0) return true;

    /* starts with an indicator. '-', '?' and ':' may start a plain scalar
     * when followed by a "safe" character: not a blank, and in flow
     * context not a flow indicator ("-123", "?x", ":x", "-foo") */
    uint8_t first = (uint8_t)s[0];
    if (oyl_is_indicator(first)) {
        if (!(first == '-' || first == '?' || first == ':')) return true;
        if (len < 2) return true;
        uint8_t next = (uint8_t)s[1];
        if (oyl_is_blank_or_break(next)) return true;
        if (flow_ctx && oyl_is_flow(next)) return true;
    }
    /* space or tab at either end */
    if (first == ' ' || first == '\t') return true;
    if (s[len - 1] == ' ' || s[len - 1] == '\t') return true;

    /* U+FEFF at the start would read as a byte order mark and be dropped */
    if (len >= 3 && first == 0xEF && (uint8_t)s[1] == 0xBB && (uint8_t)s[2] == 0xBF)
        return true;

    /* document markers */
    if (len >= 3 && (memcmp(s, "---", 3) == 0 || memcmp(s, "...", 3) == 0))
        return true;

    /* line breaks, and flow indicators in flow context */
    if (f & SC_BREAK) return true;
    if (flow_ctx && (f & SC_FLOW)) return true;

    /* ": ", ":\t", and a trailing ':' (which would read as a mapping value
     * indicator); " #" and "\t#" (a comment) */
    if (f & SC_COLON) {
        for (const char *c = memchr(s, ':', len); c; c = memchr(c + 1, ':', len - (size_t)(c + 1 - s))) {
            size_t i = (size_t)(c - s);
            if (i + 1 == len || s[i + 1] == ' ' || s[i + 1] == '\t') return true;
        }
    }
    if (f & SC_HASH) {
        for (const char *c = memchr(s, '#', len); c; c = memchr(c + 1, '#', len - (size_t)(c + 1 - s))) {
            size_t i = (size_t)(c - s);
            if (i > 0 && (s[i - 1] == ' ' || s[i - 1] == '\t')) return true;
        }
    }
    return false;
}

/* Can `s` be written as a literal block scalar and read back exactly?
 * Not with control characters (other than tab and line feed), and not
 * when the first non-empty line starts with a space: that would need an
 * indentation indicator. `f` is scalar_classes(s, len). */
static bool literal_ok(const char *s, size_t len, unsigned f) {
    if (f & SC_CTRL) return false;
    size_t i = 0;
    while (i < len && s[i] == '\n') i++;
    return !(i < len && s[i] == ' ');
}

static bool is_str_tag(oyl_str tag) {
    static const char str_tag[] = "tag:yaml.org,2002:str";
    return tag.len == sizeof(str_tag) - 1 && memcmp(tag.data, str_tag, tag.len) == 0;
}

/* A plain scalar is emitted plain whenever its text survives that: a plain
 * event means "resolve by schema", so 8080 or true stay untyped text and
 * reparse as they came in. Only a scalar tagged !!str that would resolve to
 * another type (number, bool, null) is quoted to keep it a string. */
static oyl_scalar_style choose_style(oyl_scalar_style requested,
                                     const char *val, size_t len,
                                     bool flow_ctx, bool is_key, oyl_str tag) {
    if (requested == OYL_SCALAR_DOUBLE_QUOTED) return requested;
    unsigned f = scalar_classes(val, len);
    /* Block scalars are written literal (folded values are already
     * folded, and literal keeps them exact), only in block context and
     * not as keys; otherwise double-quoted */
    if (requested == OYL_SCALAR_LITERAL || requested == OYL_SCALAR_FOLDED)
        return !flow_ctx && !is_key && literal_ok(val, len, f) ? OYL_SCALAR_LITERAL
                                                                : OYL_SCALAR_DOUBLE_QUOTED;
    /* single quotes fold line breaks; keep the value exact with escapes */
    if (requested == OYL_SCALAR_SINGLE_QUOTED)
        return f & SC_BREAK ? OYL_SCALAR_DOUBLE_QUOTED : requested;

    /* PLAIN requested — auto-detect */
    if (len == 0 && !is_str_tag(tag))
        return OYL_SCALAR_PLAIN;              /* empty (null) node */
    if (f & (SC_CTRL | SC_LF))
        return OYL_SCALAR_DOUBLE_QUOTED;      /* control characters, line feeds */
    if (!needs_quoting(val, len, flow_ctx, f)) {
        if (is_str_tag(tag) && (is_yaml_keyword(val, len) || looks_like_number(val, len)))
            return OYL_SCALAR_DOUBLE_QUOTED;
        return OYL_SCALAR_PLAIN;
    }
    /* needs quoting; without control characters or line feeds there's no
     * line break either, so a literal block scalar isn't an option */
    return OYL_SCALAR_DOUBLE_QUOTED;
}

/* ── Scalar emission ─────────────────────────────────────── */

static oyl_status emit_plain(oyl_emitter *e, const char *s, size_t len) {
    return buf_puts(e, s, len);
}

/* Quoted scalars are written in chunks of CHUNK bytes, each with room
 * reserved once for its longest escaped form */
#define CHUNK 4096

static oyl_status emit_single_quoted(oyl_emitter *e, const char *s, size_t len) {
    oyl_status st = buf_put(e, '\'');
    for (size_t i = 0; i < len && st == OYL_OK;) {
        size_t n = len - i < CHUNK ? len - i : CHUNK;
        if ((st = buf_ensure(e, 2 * n)) != OYL_OK) break;
        char *o = e->buf + e->len;
        for (size_t end = i + n; i < end; i++) {
            if (s[i] == '\'') *o++ = '\'';   /* '' */
            *o++ = s[i];
        }
        e->len = (size_t)(o - e->buf);
    }
    return st == OYL_OK ? buf_put(e, '\'') : st;
}

static oyl_status emit_double_quoted(oyl_emitter *e, const char *s, size_t len) {
    static const char hex[] = "0123456789ABCDEF";
    oyl_status st = buf_put(e, '"');
    for (size_t i = 0; i < len && st == OYL_OK;) {
        size_t n = len - i < CHUNK ? len - i : CHUNK;
        if ((st = buf_ensure(e, 4 * n)) != OYL_OK) break;   /* \xHH */
        char *o = e->buf + e->len;
        for (size_t end = i + n; i < end; i++) {
            uint8_t c = (uint8_t)s[i];
            if (c >= 0x20 && c != 0x7F && c != '\\' && c != '"') {
                *o++ = (char)c;
                continue;
            }
            *o++ = '\\';
            switch (c) {
            case '\\': *o++ = '\\'; break;
            case '"':  *o++ = '"'; break;
            case '\n': *o++ = 'n'; break;
            case '\r': *o++ = 'r'; break;
            case '\t': *o++ = 't'; break;
            case '\0': *o++ = '0'; break;
            case '\a': *o++ = 'a'; break;
            case '\b': *o++ = 'b'; break;
            default:   /* other control characters and DEL */
                *o++ = 'x';
                *o++ = hex[c >> 4];
                *o++ = hex[c & 15];
                break;
            }
        }
        e->len = (size_t)(o - e->buf);
    }
    return st == OYL_OK ? buf_put(e, '"') : st;
}

/* Write `s` as a literal block scalar, which reads back exactly. Callers
 * check literal_ok() first. Chomping follows the trailing line breaks:
 * none "|-", one "|", more (or nothing but breaks) "|+". Empty lines are
 * written without indentation. */
static oyl_status emit_block_scalar(oyl_emitter *e, const char *s, size_t len,
                                    int indent) {
    oyl_status st;
    size_t trail = 0;
    while (trail < len && s[len - 1 - trail] == '\n') trail++;
    size_t body = len - trail;

    const char *header = body == 0 ? (trail ? "|+\n" : "|-\n")
                       : trail == 0 ? "|-\n" : trail == 1 ? "|\n" : "|+\n";
    st = buf_puts(e, header, strlen(header));
    if (st != OYL_OK) return st;

    /* body lines, each ending in a line break */
    size_t pos = 0;
    while (pos < body) {
        size_t eol = pos;
        while (eol < body && s[eol] != '\n') eol++;
        if (eol > pos) {
            st = buf_indent(e, indent);
            if (st != OYL_OK) return st;
            st = buf_puts(e, s + pos, eol - pos);
            if (st != OYL_OK) return st;
        }
        st = buf_put(e, '\n');
        if (st != OYL_OK) return st;
        pos = eol + 1;
    }

    /* the first trailing break ended the last body line; kept extras are
     * empty lines (all of them when there is no body) */
    for (size_t i = body ? 1 : 0; i < trail; i++) {
        st = buf_put(e, '\n');
        if (st != OYL_OK) return st;
    }
    return OYL_OK;
}

static oyl_status emit_scalar(oyl_emitter *e, oyl_str value, oyl_scalar_style requested,
                              oyl_str tag) {
    const char *val = value.data;
    size_t vlen = value.len;
    bool flow_ctx = in_any_flow(e) || e->opts.style != OYL_EMIT_BLOCK;

    emit_ctx *ctx = top_ctx(e);
    bool is_key = ctx && (ctx->type == EMIT_CTX_BLOCK_MAP || ctx->type == EMIT_CTX_FLOW_MAP) &&
                  ctx->expect_key;
    oyl_scalar_style style = choose_style(requested, val, vlen, flow_ctx, is_key, tag);

    int indent = ctx ? ctx->indent + e->opts.indent : e->opts.indent;

    switch (style) {
    case OYL_SCALAR_PLAIN:
        if (vlen == 0) {
            /* empty node: nothing to write, except in a flow sequence
             * where "[a, ]" would drop the entry, so write ~ (null). Keep
             * the space after an anchor or tag ("&a : v", not "&a: v") */
            if (ctx && ctx->type == EMIT_CTX_FLOW_SEQ && !e->wrote_props)
                return buf_put(e, '~');
            if (!e->wrote_props && !is_key && e->len > 0 && e->buf[e->len - 1] == ' ')
                e->len--;
            return OYL_OK;
        }
        return emit_plain(e, val, vlen);
    case OYL_SCALAR_SINGLE_QUOTED:
        return emit_single_quoted(e, val, vlen);
    case OYL_SCALAR_DOUBLE_QUOTED:
        return emit_double_quoted(e, val, vlen);
    case OYL_SCALAR_LITERAL:
    case OYL_SCALAR_FOLDED:
        return emit_block_scalar(e, val, vlen, indent);
    }
    return OYL_ERR_EMIT;
}

/* ── Property emission (anchor, tag) ─────────────────────── */

static oyl_status emit_anchor(oyl_emitter *e, oyl_str anchor) {
    if (!anchor.data || anchor.len == 0) return OYL_OK;
    oyl_status st = buf_put(e, '&');
    if (st != OYL_OK) return st;
    st = buf_puts(e, anchor.data, anchor.len);
    if (st != OYL_OK) return st;
    return buf_put(e, ' ');
}

/* URI characters allowed in a verbatim tag, besides alphanumerics */
static bool is_uri_char(uint8_t c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') ||
           (c && strchr("-#;/?:@&=+$,_.!~*'()[]", c));
}

/* Characters allowed in a shorthand tag's suffix: URI characters other
 * than '!' and the flow indicators */
static bool is_tag_char(uint8_t c) {
    return is_uri_char(c) && c != '!' && !oyl_is_flow(c);
}

static bool all_tag_chars(const char *s, size_t len) {
    for (size_t i = 0; i < len; i++)
        if (!is_tag_char((uint8_t)s[i])) return false;
    return true;
}

/* Write a tag as "!!suffix" (standard tags), "!suffix" (local tags), or
 * verbatim "!<...>" with any other byte percent-encoded; the parser
 * decodes verbatim tags, so each form reads back as the same tag. */
static oyl_status emit_tag(oyl_emitter *e, oyl_str tag) {
    if (!tag.data || tag.len == 0) return OYL_OK;
    static const char prefix[] = "tag:yaml.org,2002:";
    static const size_t plen = sizeof(prefix) - 1;

    oyl_status st;
    if (tag.len > plen && memcmp(tag.data, prefix, plen) == 0 &&
        all_tag_chars(tag.data + plen, tag.len - plen)) {
        st = PUTS(e, "!!");
        if (st != OYL_OK) return st;
        st = buf_puts(e, tag.data + plen, tag.len - plen);
    } else if (tag.data[0] == '!' && all_tag_chars(tag.data + 1, tag.len - 1)) {
        st = buf_puts(e, tag.data, tag.len);
    } else {
        st = PUTS(e, "!<");
        for (size_t i = 0; i < tag.len && st == OYL_OK; i++) {
            uint8_t c = (uint8_t)tag.data[i];
            if (is_uri_char(c)) {
                st = buf_put(e, (char)c);
            } else {
                char esc[4];
                snprintf(esc, sizeof esc, "%%%02X", c);
                st = buf_puts(e, esc, 3);
            }
        }
        if (st != OYL_OK) return st;
        st = buf_put(e, '>');
    }
    if (st != OYL_OK) return st;
    return buf_put(e, ' ');
}

static bool has_props(oyl_str anchor, oyl_str tag) {
    return (anchor.data && anchor.len) || (tag.data && tag.len);
}

static oyl_status emit_props(oyl_emitter *e, oyl_str anchor, oyl_str tag) {
    e->wrote_props = has_props(anchor, tag);
    if (!e->wrote_props) return OYL_OK;
    oyl_status st = emit_anchor(e, anchor);
    if (st != OYL_OK) return st;
    return emit_tag(e, tag);
}

/* Properties of a block collection go on the line that introduces it
 * ("key: &a !!map", "- &a"), and the collection itself starts on the next
 * line; a block collection can never start on its properties' line. */
static oyl_status emit_block_collection_props(oyl_emitter *e, oyl_str anchor, oyl_str tag,
                                              bool after_key) {
    oyl_status st;
    if (!has_props(anchor, tag)) return OYL_OK;
    if (after_key) {
        st = buf_put(e, ' ');
        if (st != OYL_OK) return st;
    }
    st = emit_props(e, anchor, tag);
    if (st != OYL_OK) return st;
    if (e->len > 0 && e->buf[e->len - 1] == ' ') e->len--; /* trailing space */
    return OYL_OK;
}

/* A block collection as a mapping key must use the explicit form:
 *   ? <props>
 *     <collection>
 *   : <value>
 * Writes "?" and the key's properties; the collection follows on the next
 * lines, and the value's ':' goes on its own line (see emit_pre_node). */
static oyl_status emit_complex_key_open(oyl_emitter *e, emit_ctx *map,
                                        oyl_str anchor, oyl_str tag) {
    oyl_status st = buf_put(e, '?');
    if (st != OYL_OK) return st;
    st = emit_block_collection_props(e, anchor, tag, true);
    if (st != OYL_OK) return st;
    map->complex_key = true;
    e->after_seq_dash = false;
    return OYL_OK;
}

/* ── Pre-node prefix ─────────────────────────────────────── */

static oyl_status emit_pre_node(oyl_emitter *e, bool is_collection) {
    emit_ctx *ctx = top_ctx(e);
    oyl_status st;

    if (!ctx) {
        /* root level — newline between documents */
        if (e->len > 0 && e->buf[e->len - 1] != '\n') {
            st = buf_put(e, '\n');
            if (st != OYL_OK) return st;
        }
        return OYL_OK;
    }

    switch (ctx->type) {
    case EMIT_CTX_BLOCK_MAP:
        if (ctx->expect_key) {
            /* newline + indent before key (skip for first key after "- " or at doc start) */
            if (e->after_seq_dash) {
                /* compact mapping in sequence: first key shares "- " line */
            } else if (e->len > 0 && e->buf[e->len - 1] != '\n') {
                st = buf_put(e, '\n');
                if (st != OYL_OK) return st;
                st = buf_indent(e, ctx->indent);
                if (st != OYL_OK) return st;
            } else if (e->len > 0) {
                /* after a newline, just indent */
                st = buf_indent(e, ctx->indent);
                if (st != OYL_OK) return st;
            }
        } else {
            /* after an explicit "? key", the value indicator starts its own
             * line at the mapping's indentation */
            if (ctx->complex_key) {
                if (e->len > 0 && e->buf[e->len - 1] != '\n') {
                    st = buf_put(e, '\n');
                    if (st != OYL_OK) return st;
                }
                st = buf_indent(e, ctx->indent);
                if (st != OYL_OK) return st;
                ctx->complex_key = false;
            }
            /* ":" before a block collection value (its properties and the
             * line break follow, see block_collection_open); ": " otherwise */
            /* after an alias key, ':' needs a space: it could otherwise be
             * read as part of the alias name ("*a : v") */
            if (e->key_was_alias) {
                st = buf_put(e, ' ');
                if (st != OYL_OK) return st;
                e->key_was_alias = false;
            }
            if (is_collection) {
                st = PUTS(e, ":");
                if (st != OYL_OK) return st;
            } else {
                st = PUTS(e, ": ");
                if (st != OYL_OK) return st;
            }
            e->wrote_block_key = false;
        }
        break;

    case EMIT_CTX_BLOCK_SEQ:
        /* newline + indent + "- " (no newline when already at a line
         * start, e.g. after a block scalar, which ends with one) */
        if (e->len > 0 && e->buf[e->len - 1] != '\n') {
            st = buf_put(e, '\n');
            if (st != OYL_OK) return st;
        }
        st = buf_indent(e, ctx->indent);
        if (st != OYL_OK) return st;
        st = PUTS(e, "- ");
        if (st != OYL_OK) return st;
        e->after_seq_dash = true;
        break;

    case EMIT_CTX_FLOW_MAP:
        if (ctx->expect_key) {
            if (ctx->count > 0) {
                st = buf_puts(e, entry_sep(e), (size_t)entry_sep_len(e));
                if (st != OYL_OK) return st;
            }
        } else {
            if (e->key_was_alias) {
                st = buf_put(e, ' ');
                if (st != OYL_OK) return st;
                e->key_was_alias = false;
            }
            st = buf_puts(e, kv_sep(e), (size_t)kv_sep_len(e));
            if (st != OYL_OK) return st;
        }
        break;

    case EMIT_CTX_FLOW_SEQ:
        if (ctx->count > 0) {
            st = buf_puts(e, entry_sep(e), (size_t)entry_sep_len(e));
            if (st != OYL_OK) return st;
        }
        break;
    }

    return OYL_OK;
}

/* Advance the parent context after emitting a node */
static void advance_ctx(oyl_emitter *e) {
    emit_ctx *ctx = top_ctx(e);
    if (!ctx) return;

    if (ctx->type == EMIT_CTX_BLOCK_MAP || ctx->type == EMIT_CTX_FLOW_MAP) {
        if (ctx->expect_key) {
            ctx->expect_key = false;
            e->wrote_block_key = (ctx->type == EMIT_CTX_BLOCK_MAP);
        } else {
            ctx->expect_key = true;
            ctx->count++;
            e->wrote_block_key = false;
        }
    } else {
        /* sequence */
        ctx->count++;
    }
    e->after_seq_dash = false;
}

/* ── Nodes ───────────────────────────────────────────────── */

/* One function per kind of event, taking its fields: oyl_emit, the
 * building functions below and oyl_emit_tree all come here */

static oyl_status emit_document_start(oyl_emitter *e, bool implicit) {
    oyl_status st;
    if (!implicit) {
        if (!e->first_doc && e->len > 0 && e->buf[e->len - 1] != '\n') {
            st = buf_put(e, '\n');
            if (st != OYL_OK) return st;
        }
        st = PUTS(e, "---");
        if (st != OYL_OK) return st;
    }
    e->doc_open = true;
    e->first_doc = false;
    return OYL_OK;
}

static oyl_status emit_document_end(oyl_emitter *e, bool implicit) {
    oyl_status st;
    if (!implicit) {
        if (e->len > 0 && e->buf[e->len - 1] != '\n') {
            st = buf_put(e, '\n');
            if (st != OYL_OK) return st;
        }
        st = PUTS(e, "...\n");
        if (st != OYL_OK) return st;
    }
    e->doc_open = false;
    return OYL_OK;
}

static oyl_status emit_mapping_start(oyl_emitter *e, oyl_str anchor, oyl_str tag, bool flow) {
    oyl_status st;
    /* inside a flow collection everything must be flow */
    bool use_flow = (e->opts.style != OYL_EMIT_BLOCK) || flow || in_any_flow(e);
    emit_ctx *outer = top_ctx(e);
    bool map_value = !use_flow && outer && outer->type == EMIT_CTX_BLOCK_MAP &&
                     !outer->expect_key;

    st = emit_pre_node(e, !use_flow);
    if (st != OYL_OK) return st;

    if (!use_flow && outer && outer->type == EMIT_CTX_BLOCK_MAP && outer->expect_key) {
        st = emit_complex_key_open(e, outer, anchor, tag);
    } else if (use_flow) {
        st = emit_props(e, anchor, tag);
    } else {
        st = emit_block_collection_props(e, anchor, tag, map_value);
        /* a mapping with properties in a sequence can't share the
         * "- &a" line with its first key (the props would go to the key) */
        if (st == OYL_OK && e->after_seq_dash && has_props(anchor, tag))
            e->after_seq_dash = false;
    }
    if (st != OYL_OK) return st;

    if (use_flow) {
        st = buf_put(e, '{');
        if (st != OYL_OK) return st;
        st = push_ctx(e, EMIT_CTX_FLOW_MAP, 0);
    } else {
        emit_ctx *parent = top_ctx(e);
        int indent;
        if (parent && parent->type == EMIT_CTX_BLOCK_SEQ) {
            /* compact mapping in sequence: indent aligns with content after "- " */
            indent = parent->indent + 2;
        } else if (parent && (parent->type == EMIT_CTX_BLOCK_MAP)) {
            indent = parent->indent + e->opts.indent;
        } else {
            indent = 0;
        }
        st = push_ctx(e, EMIT_CTX_BLOCK_MAP, indent);
    }
    if (st != OYL_OK) return st;
    /* advance parent context, preserving after_seq_dash for compact mapping */
    bool saved_dash = e->after_seq_dash;
    e->stack_len--;
    advance_ctx(e);
    e->stack_len++;
    e->after_seq_dash = saved_dash;
    return OYL_OK;
}

static oyl_status emit_mapping_end(oyl_emitter *e) {
    oyl_status st;
    emit_ctx *ctx = top_ctx(e);
    if (!ctx) return OYL_ERR_EMIT;

    if (ctx->type == EMIT_CTX_FLOW_MAP) {
        st = buf_put(e, '}');
        if (st != OYL_OK) return st;
    } else if (ctx->count == 0) {
        /* empty block mapping → {} ("k: {}", "? {}") */
        if (e->len > 0 && e->buf[e->len - 1] != ' ' && e->buf[e->len - 1] != '\n') {
            st = buf_put(e, ' ');
            if (st != OYL_OK) return st;
        }
        st = PUTS(e, "{}");
        if (st != OYL_OK) return st;
    }
    pop_ctx(e);
    e->after_seq_dash = false;
    return OYL_OK;
}

static oyl_status emit_sequence_start(oyl_emitter *e, oyl_str anchor, oyl_str tag, bool flow) {
    oyl_status st;
    /* inside a flow collection everything must be flow */
    bool use_flow = (e->opts.style != OYL_EMIT_BLOCK) || flow || in_any_flow(e);
    emit_ctx *outer = top_ctx(e);
    bool map_value = !use_flow && outer && outer->type == EMIT_CTX_BLOCK_MAP &&
                     !outer->expect_key;

    st = emit_pre_node(e, !use_flow);
    if (st != OYL_OK) return st;

    if (!use_flow && outer && outer->type == EMIT_CTX_BLOCK_MAP && outer->expect_key)
        st = emit_complex_key_open(e, outer, anchor, tag);
    else
        st = use_flow ? emit_props(e, anchor, tag)
                      : emit_block_collection_props(e, anchor, tag, map_value);
    if (st != OYL_OK) return st;

    if (use_flow) {
        st = buf_put(e, '[');
        if (st != OYL_OK) return st;
        st = push_ctx(e, EMIT_CTX_FLOW_SEQ, 0);
    } else {
        emit_ctx *parent = top_ctx(e);
        int indent;
        if (parent && parent->type == EMIT_CTX_BLOCK_MAP) {
            indent = parent->indent + e->opts.indent;
        } else if (parent && parent->type == EMIT_CTX_BLOCK_SEQ) {
            indent = parent->indent + e->opts.indent;
        } else {
            indent = 0;
        }
        st = push_ctx(e, EMIT_CTX_BLOCK_SEQ, indent);
    }
    if (st != OYL_OK) return st;
    /* advance parent context, preserving after_seq_dash */
    bool saved_dash = e->after_seq_dash;
    e->stack_len--;
    advance_ctx(e);
    e->stack_len++;
    e->after_seq_dash = saved_dash;
    return OYL_OK;
}

static oyl_status emit_sequence_end(oyl_emitter *e) {
    oyl_status st;
    emit_ctx *ctx = top_ctx(e);
    if (!ctx) return OYL_ERR_EMIT;

    if (ctx->type == EMIT_CTX_FLOW_SEQ) {
        st = buf_put(e, ']');
        if (st != OYL_OK) return st;
    } else if (ctx->count == 0) {
        /* empty block sequence → [] ("k: []", "? []") */
        if (e->len > 0 && e->buf[e->len - 1] != ' ' && e->buf[e->len - 1] != '\n') {
            st = buf_put(e, ' ');
            if (st != OYL_OK) return st;
        }
        st = PUTS(e, "[]");
        if (st != OYL_OK) return st;
    }
    pop_ctx(e);
    e->after_seq_dash = false;
    return OYL_OK;
}

static oyl_status emit_scalar_node(oyl_emitter *e, oyl_str value, oyl_scalar_style style,
                                   oyl_str anchor, oyl_str tag) {
    oyl_status st = emit_pre_node(e, false);
    if (st != OYL_OK) return st;

    st = emit_props(e, anchor, tag);
    if (st != OYL_OK) return st;

    st = emit_scalar(e, value, style, tag);
    if (st != OYL_OK) return st;

    advance_ctx(e);
    return OYL_OK;
}

static oyl_status emit_alias_node(oyl_emitter *e, oyl_str name) {
    oyl_status st = emit_pre_node(e, false);
    if (st != OYL_OK) return st;
    emit_ctx *ctx = top_ctx(e);
    e->key_was_alias = ctx && (ctx->type == EMIT_CTX_BLOCK_MAP ||
                               ctx->type == EMIT_CTX_FLOW_MAP) && ctx->expect_key;

    st = buf_put(e, '*');
    if (st != OYL_OK) return st;
    st = buf_puts(e, name.data, name.len);
    if (st != OYL_OK) return st;

    advance_ctx(e);
    return OYL_OK;
}

/* ── Main emit function ──────────────────────────────────── */

oyl_status oyl_emit(oyl_emitter *e, const oyl_event *evt) {
    switch (evt->type) {
    case OYL_EVT_STREAM_START:
        e->first_doc = true;
        return OYL_OK;

    case OYL_EVT_STREAM_END:
        /* ensure trailing newline */
        if (e->len > 0 && e->buf[e->len - 1] != '\n')
            return buf_put(e, '\n');
        return OYL_OK;

    case OYL_EVT_DOC_START:      return emit_document_start(e, evt->implicit);
    case OYL_EVT_DOC_END:        return emit_document_end(e, evt->implicit);
    case OYL_EVT_MAPPING_START:  return emit_mapping_start(e, evt->anchor, evt->tag, evt->flow);
    case OYL_EVT_MAPPING_END:    return emit_mapping_end(e);
    case OYL_EVT_SEQUENCE_START: return emit_sequence_start(e, evt->anchor, evt->tag, evt->flow);
    case OYL_EVT_SEQUENCE_END:   return emit_sequence_end(e);
    case OYL_EVT_SCALAR:
        return emit_scalar_node(e, evt->value, evt->scalar_style, evt->anchor, evt->tag);
    case OYL_EVT_ALIAS:          return emit_alias_node(e, evt->value);
    default:
        return OYL_ERR_EMIT;
    }
}

/* ── Constructor / destructor ────────────────────────────── */

oyl_emitter *oyl_emitter_new(oyl_arena *a) {
    oyl_emitter *e = malloc(sizeof(*e));
    if (!e) return NULL;
    memset(e, 0, sizeof(*e));

    e->opts = (emit_opts){ OYL_EMIT_BLOCK, 2 };
    e->arena = a;

    e->cap = EMIT_INIT_CAP;
    e->buf = malloc(e->cap);
    if (!e->buf) { free(e); return NULL; }

    e->stack_cap = 8;
    e->stack = malloc((size_t)e->stack_cap * sizeof(emit_ctx));
    if (!e->stack) { free(e->buf); free(e); return NULL; }

    e->first_doc = true;
    return e;
}

void oyl_emitter_set_style(oyl_emitter *e, oyl_emit_style style) {
    if (style == OYL_EMIT_BLOCK || style == OYL_EMIT_FLOW || style == OYL_EMIT_MINIMAL)
        e->opts.style = style;
}

void oyl_emitter_set_indent(oyl_emitter *e, int indent) {
    if (indent < 1) indent = 1;
    if (indent > 10) indent = 10;
    e->opts.indent = indent;
}

/* ── Event constructors ──────────────────────────────────── */

oyl_status oyl_emit_stream_start(oyl_emitter *e) {
    oyl_event evt = { .type = OYL_EVT_STREAM_START };
    return oyl_emit(e, &evt);
}

oyl_status oyl_emit_stream_end(oyl_emitter *e) {
    oyl_event evt = { .type = OYL_EVT_STREAM_END };
    return oyl_emit(e, &evt);
}

oyl_status oyl_emit_document_start(oyl_emitter *e, bool implicit) {
    return emit_document_start(e, implicit);
}

oyl_status oyl_emit_document_end(oyl_emitter *e, bool implicit) {
    return emit_document_end(e, implicit);
}

oyl_status oyl_emit_scalar(oyl_emitter *e, oyl_str value, oyl_scalar_style style,
                           oyl_str anchor, oyl_str tag) {
    return emit_scalar_node(e, value, style, anchor, tag);
}

oyl_status oyl_emit_alias(oyl_emitter *e, oyl_str name) {
    return emit_alias_node(e, name);
}

oyl_status oyl_emit_mapping_start(oyl_emitter *e, oyl_str anchor, oyl_str tag, bool flow) {
    return emit_mapping_start(e, anchor, tag, flow);
}

oyl_status oyl_emit_mapping_end(oyl_emitter *e) {
    return emit_mapping_end(e);
}

oyl_status oyl_emit_sequence_start(oyl_emitter *e, oyl_str anchor, oyl_str tag, bool flow) {
    return emit_sequence_start(e, anchor, tag, flow);
}

oyl_status oyl_emit_sequence_end(oyl_emitter *e) {
    return emit_sequence_end(e);
}

oyl_str oyl_emitter_output(oyl_emitter *e) {
    if (!e || !e->buf || e->len == 0) return OYL_STR_NULL;
    char *copy = oyl_arena_dup(e->arena, e->buf, e->len);
    if (!copy) return OYL_STR_NULL;
    return (oyl_str){copy, e->len};
}

void oyl_emitter_free(oyl_emitter *e) {
    if (!e) return;
    free(e->buf);
    free(e->stack);
    free(e);
}
