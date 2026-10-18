/*
 * oyl_schema.c — tag schemas: the tags and values of scalars
 *
 * A schema is an ordered list of rules, each a tag and a parser. A plain
 * scalar resolves to the first rule whose parser accepts it, and that
 * parser gives its value (oyl_schema_value). Presets:
 *   - Failsafe:   everything is str/seq/map
 *   - JSON:       null, true/false, int, float (with Core's number rules)
 *   - Core:       YAML 1.2 Core: extended booleans and nulls, 0x/0o ints
 *   - go-yaml v2: as Kubernetes reads YAML, with YAML 1.1's types
 *
 * The builder adds words, the built-in int and float parsers with dialect
 * flags, and custom parsers.
 */

#include "oyl_internal.h"
#include <errno.h>
#include <locale.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>

/* ── Tag constant definitions ────────────────────────────── */

static const char TAG_NULL[]      = "tag:yaml.org,2002:null";
static const char TAG_BOOL[]      = "tag:yaml.org,2002:bool";
static const char TAG_INT[]       = "tag:yaml.org,2002:int";
static const char TAG_FLOAT[]     = "tag:yaml.org,2002:float";
static const char TAG_STR[]       = "tag:yaml.org,2002:str";
static const char TAG_SEQ[]       = "tag:yaml.org,2002:seq";
static const char TAG_MAP[]       = "tag:yaml.org,2002:map";
static const char TAG_MERGE[]     = "tag:yaml.org,2002:merge";
static const char TAG_TIMESTAMP[] = "tag:yaml.org,2002:timestamp";
static const char TAG_BINARY[]    = "tag:yaml.org,2002:binary";
static const char TAG_VALUE[]     = "tag:yaml.org,2002:value";

#define TAG(name) { TAG_##name, sizeof(TAG_##name) - 1 }

const oyl_str OYL_TAG_NULL      = TAG(NULL);
const oyl_str OYL_TAG_BOOL      = TAG(BOOL);
const oyl_str OYL_TAG_INT       = TAG(INT);
const oyl_str OYL_TAG_FLOAT     = TAG(FLOAT);
const oyl_str OYL_TAG_STR       = TAG(STR);
const oyl_str OYL_TAG_SEQ       = TAG(SEQ);
const oyl_str OYL_TAG_MAP       = TAG(MAP);
const oyl_str OYL_TAG_MERGE     = TAG(MERGE);
const oyl_str OYL_TAG_TIMESTAMP = TAG(TIMESTAMP);
const oyl_str OYL_TAG_BINARY    = TAG(BINARY);
const oyl_str OYL_TAG_VALUE     = TAG(VALUE);

static bool str_eq(oyl_str a, oyl_str b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.data, b.data, a.len) == 0);
}

/* ── Reading past underscores ────────────────────────────── */

/* A cursor over text that can skip underscores, as if they weren't there
 * (go-yaml v2 removes them from a number before parsing it) */
typedef struct {
    const char *s;
    size_t      len, i;
    bool        skip;
} cursor;

static int cur_peek(cursor *c) {
    while (c->skip && c->i < c->len && c->s[c->i] == '_') c->i++;
    return c->i < c->len ? (unsigned char)c->s[c->i] : -1;
}

static void cur_next(cursor *c) { c->i++; }

static int digit_value(int ch, int base) {
    int d = ch >= '0' && ch <= '9' ? ch - '0'
          : ch >= 'a' && ch <= 'z' ? ch - 'a' + 10
          : ch >= 'A' && ch <= 'Z' ? ch - 'A' + 10
          : 99;
    return d < base ? d : -1;
}

/* ── The built-in int parser ─────────────────────────────── */

/*
 * [-+]? ( D+ | 0x X+ | 0o O+ | 0b B+ | 0 O+ ), each part as `flags`
 * allow. A value beyond 64 bits (beyond int64 with a sign, as Go's
 * ParseInt and ParseUint see it) is declined under OYL_INT_RANGE and
 * otherwise kept as text. The Core flags accept what YAML 1.2's Core int
 * does, and a sign before 0x or 0o.
 */
/* A magnitude plus a sign as an int value: INT, UINT, or text if too big */
static void int_value(uint64_t mag, bool neg, bool big, const char *s, size_t len,
                      oyl_value *out) {
    if (!big && neg && mag > (uint64_t)INT64_MAX + 1) big = true;
    if (big)
        *out = (oyl_value){ .kind = OYL_VALUE_STR, .as = { .s = { s, len } } };
    else if (neg)
        *out = (oyl_value){ .kind = OYL_VALUE_INT,
                            .as = { .i = mag == (uint64_t)INT64_MAX + 1 ? INT64_MIN : -(int64_t)mag } };
    else if (mag <= INT64_MAX)
        *out = (oyl_value){ .kind = OYL_VALUE_INT, .as = { .i = (int64_t)mag } };
    else
        *out = (oyl_value){ .kind = OYL_VALUE_UINT, .as = { .u = mag } };
}

static bool mul_add(uint64_t *v, uint64_t m, uint64_t a) {
    return !__builtin_mul_overflow(*v, m, v) && !__builtin_add_overflow(*v, a, v);
}

/* YAML 1.1's base 60 int, [-+]?[1-9][0-9_]*(:[0-5]?[0-9])+: 190:20:30 is
 * 190 * 3600 + 20 * 60 + 30 */
static bool parse_int60(const char *s, size_t len, oyl_value *out) {
    size_t i = s[0] == '+' || s[0] == '-';
    bool neg = s[0] == '-', big = false;
    if (i >= len || s[i] < '1' || s[i] > '9') return false;
    uint64_t v = 0;
    for (; i < len && s[i] != ':'; i++) {
        if (s[i] == '_') continue;
        if (!isdigit((unsigned char)s[i])) return false;
        if (!big && !mul_add(&v, 10, (uint64_t)(s[i] - '0'))) big = true;
    }
    int parts = 0;
    while (i < len) {   /* at a ':' */
        size_t start = ++i;
        unsigned d = 0;
        while (i < len && i - start < 2 && isdigit((unsigned char)s[i])) d = d * 10 + (unsigned)(s[i++] - '0');
        if (i == start || (i - start == 2 && s[start] > '5') || (i < len && s[i] != ':')) return false;
        if (!big && !mul_add(&v, 60, d)) big = true;
        parts++;
    }
    if (!parts) return false;
    if (out) int_value(v, neg, big, s, len, out);
    return true;
}

static bool parse_int(const char *s, size_t len, unsigned flags, oyl_value *out) {
    bool digit_underscores = (flags & OYL_INT_DIGIT_UNDERSCORE) != 0;
    bool underscores = (flags & OYL_INT_UNDERSCORE) != 0 || digit_underscores;
    if (len == 0 || (underscores && s[0] == '_')) return false;
    if ((flags & OYL_INT_BASE60) && memchr(s, ':', len)) return parse_int60(s, len, out);
    /* YAML 1.1: no underscore right after the sign */
    if (digit_underscores && len > 1 && (s[0] == '+' || s[0] == '-') && s[1] == '_') return false;
    cursor c = { s, len, 0, underscores };
    int ch = cur_peek(&c);
    bool sign = false, neg = false;
    if (ch == '+' || ch == '-') {
        if (!(flags & OYL_INT_SIGN)) return false;
        sign = true;
        neg = ch == '-';
        cur_next(&c);
        ch = cur_peek(&c);
    }
    if (ch < '0' || ch > '9') return false;

    int base = 10;
    if (ch == '0') {
        cursor after = c;
        cur_next(&after);
        /* YAML 1.1 keeps a prefix whole (0_x1F is no hex) */
        int p = digit_underscores ? (after.i < len ? (unsigned char)s[after.i] : -1)
                                  : cur_peek(&after);
        bool any_case = (flags & OYL_INT_PREFIX_CASE) != 0;
        int prefix = (p == 'x' || (any_case && p == 'X')) && (flags & OYL_INT_HEX) ? 16
                   : (p == 'o' || (any_case && p == 'O')) && (flags & OYL_INT_OCT) ? 8
                   : (p == 'b' || (any_case && p == 'B')) && (flags & OYL_INT_BIN) ? 2
                   : 0;
        if (prefix) {
            if (sign && !(flags & OYL_INT_SIGN_PREFIX)) return false;
            base = prefix;
            c = after;
            cur_next(&c);
            if (digit_value(cur_peek(&c), base) < 0) return false;
        } else if (p >= 0 && (flags & OYL_INT_OCT_ZERO)) {
            base = 8;   /* 017; a lone 0 stays decimal */
        }
    }

    /* resolving needs only the syntax, unless the range decides */
    if (!out && !(flags & OYL_INT_RANGE)) {
        for (ch = cur_peek(&c); ch >= 0; cur_next(&c), ch = cur_peek(&c))
            if (digit_value(ch, base) < 0) return false;
        return true;
    }

    uint64_t mag = 0;
    bool big = false;
    for (ch = cur_peek(&c); ch >= 0; cur_next(&c), ch = cur_peek(&c)) {
        int d = digit_value(ch, base);
        if (d < 0) return false;
        if (!big && (__builtin_mul_overflow(mag, (uint64_t)base, &mag) ||
                     __builtin_add_overflow(mag, (uint64_t)d, &mag)))
            big = true;
    }
    if (!big && neg && mag > (uint64_t)INT64_MAX + 1) big = true;
    if (!big && sign && !neg && mag > INT64_MAX && (flags & OYL_INT_RANGE)) big = true;
    if (big && (flags & OYL_INT_RANGE)) return false;
    if (out) int_value(mag, neg, big, s, len, out);
    return true;
}

/* ── The built-in float parser ───────────────────────────── */

/* One of a four-byte word's three spellings: ".inf", ".Inf", ".INF" */
static bool spelled(const char *s, size_t len, const char *lower,
                    const char *capital, const char *upper) {
    return len == 4 && (memcmp(s, lower, 4) == 0 || memcmp(s, capital, 4) == 0 ||
                        memcmp(s, upper, 4) == 0);
}

/* The double a validated float literal stands for, by strtod in a copy
 * without underscores and with the locale's decimal point. 1 on success,
 * 0 when it overflows under OYL_FLOAT_RANGE, -1 out of memory. */
static int float_value(const char *s, size_t len, bool underscores, unsigned flags,
                       double *f) {
    char stack[64];
    char *buf = len < sizeof stack ? stack : malloc(len + 1);
    if (!buf) return -1;
    char point = *localeconv()->decimal_point;
    size_t n = 0;
    for (size_t i = 0; i < len; i++) {
        if (underscores && s[i] == '_') continue;
        buf[n++] = s[i] == '.' ? point : s[i];
    }
    buf[n] = '\0';
    errno = 0;
    *f = strtod(buf, NULL);
    bool overflow = errno == ERANGE && isinf(*f);
    if (buf != stack) free(buf);
    return overflow && (flags & OYL_FLOAT_RANGE) ? 0 : 1;
}

/* YAML 1.1's base 60 float, [-+]?[0-9][0-9_]*(:[0-5]?[0-9])+\.[0-9_]*:
 * 20:30.15 is 20 * 60 + 30.15. The value as PyYAML computes it: each part
 * read as a float, then summed from the right times powers of 60. */
static int parse_float60(const char *s, size_t len, oyl_value *out) {
    size_t i = s[0] == '+' || s[0] == '-';
    if (i >= len || !isdigit((unsigned char)s[i])) return 0;
    size_t starts[64], ends[64];
    int parts = 0;
    starts[0] = i;
    for (; i < len && s[i] != ':'; i++)
        if (s[i] != '_' && !isdigit((unsigned char)s[i])) return 0;
    if (i >= len) return 0;
    ends[parts++] = i;
    bool fraction = false;
    while (i < len) {   /* at a ':' */
        size_t start = ++i;
        while (i < len && i - start < 2 && isdigit((unsigned char)s[i])) i++;
        if (i == start || (i - start == 2 && s[start] > '5')) return 0;
        if (i < len && s[i] == '.') {   /* the fraction ends it */
            for (i++; i < len; i++)
                if (s[i] != '_' && !isdigit((unsigned char)s[i])) return 0;
            fraction = true;
        } else if (i < len && s[i] != ':') {
            return 0;
        }
        if (parts == 64) return 0;
        starts[parts] = start;
        ends[parts++] = i;
    }
    if (!fraction) return 0;   /* a base 60 int, not a float */
    if (!out) return 1;
    double value = 0, base = 1;
    for (int k = parts - 1; k >= 0; k--) {
        double part;
        if (float_value(s + starts[k], ends[k] - starts[k], true, 0, &part) < 0) return -1;
        value += part * base;
        base *= 60;
    }
    *out = (oyl_value){ .kind = OYL_VALUE_FLOAT, .as = { .f = s[0] == '-' ? -value : value } };
    return 1;
}

/*
 * [-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)? with a dot or an
 * exponent, or digits alone under OYL_FLOAT_DIGITS. Under
 * OYL_FLOAT_SPECIALS also [-+]?(\.inf|\.Inf|\.INF) and \.nan|\.NaN|\.NAN.
 * YAML 1.1's flags require a dot and an exponent's sign, allow underscores
 * after the first digit, and add base 60.
 * 1 accepted, 0 declined, -1 out of memory.
 */
static int parse_float(const char *s, size_t len, unsigned flags, oyl_value *out) {
    if (len == 0) return 0;
    if ((flags & OYL_FLOAT_BASE60) && memchr(s, ':', len)) return parse_float60(s, len, out);
    if (flags & OYL_FLOAT_SPECIALS) {
        if (spelled(s, len, ".nan", ".NaN", ".NAN")) {
            if (out) *out = (oyl_value){ .kind = OYL_VALUE_FLOAT, .as = { .f = NAN } };
            return 1;
        }
        size_t k = s[0] == '+' || s[0] == '-';
        if (spelled(s + k, len - k, ".inf", ".Inf", ".INF")) {
            if (out)
                *out = (oyl_value){ .kind = OYL_VALUE_FLOAT,
                                    .as = { .f = s[0] == '-' ? -INFINITY : INFINITY } };
            return 1;
        }
    }

    /* go-yaml v2 removes underscores from what starts with a digit or sign,
     * and hands the rest to Go's ParseFloat, which allows them only
     * between digits */
    bool underscores = false;
    if ((flags & OYL_FLOAT_DIGIT_UNDERSCORE) && memchr(s, '_', len)) {
        /* YAML 1.1: after the first digit and in the fraction, not before
         * the first digit nor in the exponent */
        size_t k = s[0] == '+' || s[0] == '-';
        if (k < len && s[k] == '_') return 0;
        for (bool exponent = false; k < len; k++) {
            if (s[k] == 'e' || s[k] == 'E') exponent = true;
            else if (s[k] == '_' && exponent) return 0;
        }
        underscores = true;
    } else if ((flags & OYL_FLOAT_UNDERSCORE) && memchr(s, '_', len)) {
        underscores = isdigit((unsigned char)s[0]) || s[0] == '+' || s[0] == '-';
        if (!underscores) {
            for (size_t k = 0; k < len; k++)
                if (s[k] == '_' && (k == 0 || k + 1 == len || !isdigit((unsigned char)s[k - 1]) ||
                                    !isdigit((unsigned char)s[k + 1])))
                    return 0;
            underscores = true;
        }
    }
    cursor c = { s, len, 0, underscores };
    int ch = cur_peek(&c);
    if (ch == '+' || ch == '-') {
        cur_next(&c);
        ch = cur_peek(&c);
        if (ch == '.' && (flags & OYL_FLOAT_DOT_NO_SIGN)) return 0;
    }
    bool digits = false, dot = false, exp = false;
    for (; ch >= '0' && ch <= '9'; cur_next(&c), ch = cur_peek(&c)) digits = true;
    if (ch == '.') {
        dot = true;
        bool frac = false;
        for (cur_next(&c), ch = cur_peek(&c); ch >= '0' && ch <= '9'; cur_next(&c), ch = cur_peek(&c))
            frac = true;
        if (!digits && !frac) return 0;
        digits = true;
    }
    if (!digits) return 0;
    if (ch == 'e' || ch == 'E') {
        exp = true;
        cur_next(&c);
        ch = cur_peek(&c);
        if (ch == '+' || ch == '-') { cur_next(&c); ch = cur_peek(&c); }
        else if (flags & OYL_FLOAT_EXP_SIGN) return 0;
        if (ch < '0' || ch > '9') return 0;
        for (; ch >= '0' && ch <= '9'; cur_next(&c), ch = cur_peek(&c))
            ;
    }
    if (ch >= 0) return 0;
    if (!dot && (flags & OYL_FLOAT_NEED_DOT)) return 0;
    if (!dot && !exp && !(flags & OYL_FLOAT_DIGITS)) return 0;
    if (!out && !(flags & OYL_FLOAT_RANGE)) return 1;

    double f;
    int r = float_value(s, len, underscores, flags, &f);
    if (r == 1 && out) *out = (oyl_value){ .kind = OYL_VALUE_FLOAT, .as = { .f = f } };
    return r;
}

/* ── go-yaml v2's timestamps ─────────────────────────────── */

/* One or two digits at s[*i], as Go's time.Parse reads an unpadded field */
static bool small_num(const char *s, size_t len, size_t *i, int *n) {
    if (*i >= len || !isdigit((unsigned char)s[*i])) return false;
    *n = s[(*i)++] - '0';
    if (*i < len && isdigit((unsigned char)s[*i])) *n = *n * 10 + (s[(*i)++] - '0');
    return true;
}

static int days_in(int month, int year) {
    static const int days[] = { 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31 };
    bool leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    return month == 2 && leap ? 29 : days[month - 1];
}

/*
 * go-yaml v2 tries Go's time.Parse with four layouts:
 *   2006-1-2T15:4:5.999999999Z07:00   (and a lowercase t)
 *   2006-1-2 15:4:5.999999999
 *   2006-1-2
 * so a four-digit year, a one- or two-digit month, day, hour, minute and
 * second, each in range; any number of fraction digits after a . or a ,;
 * after a T a zone, Z or ±hh:mm, and in place of the space any run of
 * spaces.
 */
static bool parse_timestamp(const char *s, size_t len) {
    size_t i = 0;
    int year = 0, month, day, hour, minute, second;
    for (; i < 4; i++) {
        if (i >= len || !isdigit((unsigned char)s[i])) return false;
        year = year * 10 + (s[i] - '0');
    }
    if (i >= len || s[i++] != '-') return false;
    if (!small_num(s, len, &i, &month) || month < 1 || month > 12) return false;
    if (i >= len || s[i++] != '-') return false;
    if (!small_num(s, len, &i, &day) || day < 1 || day > days_in(month, year)) return false;
    if (i == len) return true;

    char sep = s[i++];
    if (sep != 'T' && sep != 't' && sep != ' ') return false;
    if (sep == ' ')
        while (i < len && s[i] == ' ') i++;   /* time.Parse: a run of spaces is one */
    if (!small_num(s, len, &i, &hour) || hour > 23) return false;
    if (i >= len || s[i++] != ':') return false;
    if (!small_num(s, len, &i, &minute) || minute > 59) return false;
    if (i >= len || s[i++] != ':') return false;
    if (!small_num(s, len, &i, &second) || second > 59) return false;
    if (i + 1 < len && (s[i] == '.' || s[i] == ',') && isdigit((unsigned char)s[i + 1]))
        for (i++; i < len && isdigit((unsigned char)s[i]); i++)
            ;
    if (sep == ' ') return i == len;

    if (i < len && s[i] == 'Z') return i + 1 == len;
    if (len - i != 6 || (s[i] != '+' && s[i] != '-') || s[i + 3] != ':') return false;
    for (size_t k = 1; k < 6; k++)
        if (k != 3 && !isdigit((unsigned char)s[i + k])) return false;
    int zh = (s[i + 1] - '0') * 10 + (s[i + 2] - '0');
    int zm = (s[i + 4] - '0') * 10 + (s[i + 5] - '0');
    return zh <= 24 && zm <= 60;   /* Go allows offsets of 24 hours or 60 minutes */
}

/* YAML 1.1's timestamps (yaml.org/type/timestamp), as PyYAML's pattern has
 * them, which allows space before a zone as the type's own example does:
 *   YYYY-MM-DD
 *   YYYY-M-D([Tt]|[ \t]+)H:MM:SS(.F*)?([ \t]*(Z|[-+]H(:MM)?))?
 * The pattern checks no ranges. */
static bool digits_at(const char *s, size_t len, size_t *i, size_t min, size_t max) {
    size_t n = 0;
    while (*i < len && n < max && isdigit((unsigned char)s[*i])) { (*i)++; n++; }
    return n >= min;
}

static bool parse_timestamp11(const char *s, size_t len) {
    size_t i = 0;
    if (!digits_at(s, len, &i, 4, 4) || i >= len || s[i++] != '-') return false;
    size_t m = i;
    if (!digits_at(s, len, &i, 1, 2) || i >= len || s[i++] != '-') return false;
    size_t d = i;
    if (!digits_at(s, len, &i, 1, 2)) return false;
    if (i == len) return i - d == 2 && d - m == 3;   /* the date alone: MM and DD */
    if (s[i] == 'T' || s[i] == 't') {
        i++;
    } else if (s[i] == ' ' || s[i] == '\t') {
        while (i < len && (s[i] == ' ' || s[i] == '\t')) i++;
    } else {
        return false;
    }
    if (!digits_at(s, len, &i, 1, 2) || i >= len || s[i++] != ':') return false;
    if (!digits_at(s, len, &i, 2, 2) || i >= len || s[i++] != ':') return false;
    if (!digits_at(s, len, &i, 2, 2)) return false;
    if (i < len && s[i] == '.') { i++; digits_at(s, len, &i, 0, SIZE_MAX); }
    if (i == len) return true;
    while (i < len && (s[i] == ' ' || s[i] == '\t')) i++;
    if (i < len && s[i] == 'Z') return i + 1 == len;
    if (i >= len || (s[i] != '+' && s[i] != '-')) return false;
    i++;
    if (!digits_at(s, len, &i, 1, 2)) return false;
    if (i < len && s[i] == ':') { i++; if (!digits_at(s, len, &i, 2, 2)) return false; }
    return i == len;
}

/* ── Rules ───────────────────────────────────────────────── */

static bool word_eq(const char *pattern, size_t plen, oyl_str text, bool icase) {
    if (text.len != plen) return false;
    if (!icase) return plen == 0 || memcmp(text.data, pattern, plen) == 0;
    for (size_t j = 0; j < plen; j++)
        if (tolower((unsigned char)text.data[j]) != tolower((unsigned char)pattern[j]))
            return false;
    return true;
}

/* Rules that can't match, ruled out inline before the call: a word of
 * another length or first byte (most of a schema's rules are words), or a
 * number or timestamp for text that starts like neither */
static inline bool word_misses(const oyl_schema_rule *r, oyl_str text) {
    if (r->kind <= RULE_WORD_ICASE)
        return text.len != r->plen ||
               (r->kind == RULE_WORD && text.len && text.data[0] != r->pattern[0]);
    if (r->kind <= RULE_TIMESTAMP && text.len) {
        unsigned char c = (unsigned char)text.data[0];
        return !((c >= '0' && c <= '9') || c == '+' || c == '-' || c == '.');
    }
    return false;
}

/* Does rule `r` accept `text`? If so and `out` isn't NULL, fill in the
 * value. 1 accepted, 0 declined, -1 out of memory. */
static int rule_parse(const oyl_schema_rule *r, oyl_str text, oyl_value *out) {
    switch (r->kind) {
    case RULE_WORD:
    case RULE_WORD_ICASE:
        if (!word_eq(r->pattern, r->plen, text, r->kind == RULE_WORD_ICASE)) return 0;
        if (out) {
            *out = r->value;
            if (out->kind == OYL_VALUE_STR) out->as.s = text;
        }
        return 1;
    case RULE_INT:
        return parse_int(text.data, text.len, r->flags, out);
    case RULE_FLOAT:
        return parse_float(text.data, text.len, r->flags, out);
    case RULE_TIMESTAMP:   /* flags 1: YAML 1.1's forms, else go-yaml v2's */
        if (!(r->flags ? parse_timestamp11(text.data, text.len)
                       : parse_timestamp(text.data, text.len)))
            return 0;
        if (out) *out = (oyl_value){ .kind = OYL_VALUE_STR, .as = { .s = text } };
        return 1;
    case RULE_CUSTOM: {
        oyl_value ignored;
        return r->parse(text, out ? out : &ignored, r->user) ? 1 : 0;
    }
    case RULE_NONE:
        break;
    }
    return 0;
}

/* Can rule r accept text starting with byte c (256: empty text)? As the
 * first-byte table and word_misses decide */
static bool may_start(const oyl_schema_rule *r, int c) {
    switch (r->kind) {
    case RULE_WORD:
    case RULE_WORD_ICASE: {
        if (c == 256 || r->plen == 0) return c == 256 && r->plen == 0;
        int p = (unsigned char)r->pattern[0];
        return c == p || (r->kind == RULE_WORD_ICASE &&
                          (c == (unsigned char)tolower(p) || c == (unsigned char)toupper(p)));
    }
    case RULE_INT:
    case RULE_FLOAT:
    case RULE_TIMESTAMP:
        return c == 256 || (c >= '0' && c <= '9') || c == '+' || c == '-' || c == '.';
    case RULE_CUSTOM:
        return true;
    case RULE_NONE:
        break;
    }
    return false;
}

/* Fill a schema's candidate rules by first byte (into `cand`, `cap` long,
 * or with cand NULL just count them). Returns how many, or -1 if there
 * are too many rules or candidates. */
static long fill_cand(const oyl_schema_rule *rules, int n, uint16_t *at, uint8_t *cand, size_t cap) {
    if (n > 255) return -1;
    size_t k = 0;
    for (int c = 0; c <= 256; c++) {
        if (at) at[c] = (uint16_t)k;
        for (int i = 0; i < n; i++) {
            if (!may_start(&rules[i], c)) continue;
            if (cand && k >= cap) return -1;
            if (cand) cand[k] = (uint8_t)i;
            k++;
        }
    }
    if (k > UINT16_MAX) return -1;
    if (at) at[257] = (uint16_t)k;
    return (long)k;
}

/* The first rule that accepts untagged plain `text`: its index, -1 for
 * none, or -2 if a rule ran out of memory giving its value in `out` */
static int match(const oyl_schema *s, oyl_str text, oyl_value *out) {
    if (text.len && s->first && !s->first[(unsigned char)text.data[0]]) return -1;
    if (s->cand_at && s->cand_at[257]) {
        int c = text.len ? (unsigned char)text.data[0] : 256;
        for (unsigned j = s->cand_at[c]; j < s->cand_at[c + 1]; j++) {
            const oyl_schema_rule *r = &s->rules[s->cand[j]];
            if (word_misses(r, text)) continue;
            int x = rule_parse(r, text, out);
            if (x > 0) return s->cand[j];
            if (x < 0 && out) return -2;
        }
        return -1;
    }
    for (int i = 0; i < s->rule_count; i++) {
        const oyl_schema_rule *r = &s->rules[i];
        if (word_misses(r, text)) continue;
        int x = rule_parse(r, text, out);
        if (x > 0) return i;
        if (x < 0 && out) return -2;
    }
    return -1;
}

/* ── Resolution and values ───────────────────────────────── */

oyl_str oyl_schema_resolve(const oyl_schema *schema, oyl_str value,
                           oyl_scalar_style style) {
    /* quoted scalars always resolve to default_quoted_tag (str) */
    if (style != OYL_SCALAR_PLAIN)
        return schema->default_quoted_tag;
    /* resolution never needs memory: a float is only copied when its range
     * decides, and then a failed copy declines */
    int i = match(schema, value, NULL);
    return i >= 0 ? schema->rules[i].tag : schema->default_plain_tag;
}

int oyl_schema_rule_of(const oyl_schema *schema, oyl_str text) {
    return match(schema, text, NULL);
}

int oyl_schema_rule_value(const oyl_schema *schema, int rule, oyl_str text, oyl_value *out) {
    return rule_parse(&schema->rules[rule], text, out);
}

static void text_value(oyl_value *out, oyl_str text) {
    *out = (oyl_value){ .kind = OYL_VALUE_STR, .as = { .s = text } };
}

oyl_status oyl_schema_value(const oyl_schema *schema, const oyl_event *scalar,
                            oyl_value *out) {
    if (!schema || !scalar || !out || scalar->type != OYL_EVT_SCALAR) return OYL_ERR_INPUT;
    return oyl_schema_value_of(schema, scalar->value, scalar->scalar_style, scalar->tag, out);
}

oyl_status oyl_schema_value_of(const oyl_schema *schema, oyl_str text, oyl_scalar_style style,
                               oyl_str tag, oyl_value *out) {
    if (!schema || !out || (!text.data && text.len) || (!tag.data && tag.len)) return OYL_ERR_INPUT;
    if (tag.len == 0) {
        if (style == OYL_SCALAR_PLAIN) {
            int i = match(schema, text, out);
            if (i == -2) return OYL_ERR_MEMORY;
            if (i >= 0) return OYL_OK;
        }
        text_value(out, text);
        return OYL_OK;
    }
    bool bound = false;
    for (int i = 0; i < schema->rule_count; i++) {
        if (!str_eq(schema->rules[i].tag, tag)) continue;
        bound = true;
        int r = rule_parse(&schema->rules[i], text, out);
        if (r < 0) return OYL_ERR_MEMORY;
        if (r) return OYL_OK;
    }
    if (bound) return OYL_ERR_PARSE;
    text_value(out, text);
    return OYL_OK;
}

/* ── Presets ─────────────────────────────────────────────── */

#define V_NULL   { .kind = OYL_VALUE_NULL }
#define V_TRUE   { .kind = OYL_VALUE_BOOL, .as = { .b = true } }
#define V_FALSE  { .kind = OYL_VALUE_BOOL, .as = { .b = false } }
#define WORD(w, t, v)     { .kind = RULE_WORD, .pattern = (w), .plen = sizeof(w) - 1, \
                            .tag = t, .value = v }
#define NULL_WORD(w)      WORD(w, TAG(NULL), V_NULL)
#define TRUE_WORD(w)      WORD(w, TAG(BOOL), V_TRUE)
#define FALSE_WORD(w)     WORD(w, TAG(BOOL), V_FALSE)
#define INT_RULE(f)       { .kind = RULE_INT, .tag = TAG(INT), .flags = (f) }
#define FLOAT_RULE(f)     { .kind = RULE_FLOAT, .tag = TAG(FLOAT), .flags = (f) }
#define TIMESTAMP_RULE    { .kind = RULE_TIMESTAMP, .tag = TAG(TIMESTAMP) }
#define TIMESTAMP11_RULE  { .kind = RULE_TIMESTAMP, .tag = TAG(TIMESTAMP), .flags = 1 }
#define V_TEXT   { .kind = OYL_VALUE_STR }

#define DEFAULT_TAGS \
    .default_plain_tag  = TAG(STR), \
    .default_quoted_tag = TAG(STR), \
    .default_seq_tag    = TAG(SEQ), \
    .default_map_tag    = TAG(MAP)

/* The presets' candidate rules by first byte are filled in when the library
 * loads, where the compiler can arrange it; until then (cand_at[257] 0),
 * and elsewhere, every rule is tried */
#if defined(__GNUC__) || defined(__clang__)
#  define CAND(r) static uint16_t r##_at[258]; static uint8_t r##_cand[512];
#  define PRESET(r, f) { .rules = r, .rule_count = sizeof(r) / sizeof(r[0]), DEFAULT_TAGS, \
                         .first = f, .cand_at = r##_at, .cand = r##_cand }
#else
#  define CAND(r)
#  define PRESET(r, f) { .rules = r, .rule_count = sizeof(r) / sizeof(r[0]), DEFAULT_TAGS, .first = f }
#endif

/* The first bytes the presets' rules accept, for the shortcut in
 * resolution: numbers start with a digit, sign or dot, the rest are words */
#define FIRST_NUMBER ['0'] = 1, ['1'] = 1, ['2'] = 1, ['3'] = 1, ['4'] = 1, ['5'] = 1, \
                     ['6'] = 1, ['7'] = 1, ['8'] = 1, ['9'] = 1, ['+'] = 1, ['-'] = 1, ['.'] = 1
static const bool json_first[256] = { FIRST_NUMBER, ['n'] = 1, ['t'] = 1, ['f'] = 1 };
static const bool core_first[256] = { FIRST_NUMBER, ['n'] = 1, ['N'] = 1, ['~'] = 1, ['t'] = 1,
                                      ['T'] = 1, ['f'] = 1, ['F'] = 1 };
static const bool goyaml2_first[256] = { FIRST_NUMBER, ['~'] = 1, ['n'] = 1, ['N'] = 1, ['y'] = 1,
                                         ['Y'] = 1, ['t'] = 1, ['T'] = 1, ['o'] = 1, ['O'] = 1,
                                         ['f'] = 1, ['F'] = 1 };
static const bool yaml11_first[256] = { FIRST_NUMBER, ['~'] = 1, ['n'] = 1, ['N'] = 1, ['y'] = 1,
                                        ['Y'] = 1, ['t'] = 1, ['T'] = 1, ['o'] = 1, ['O'] = 1,
                                        ['f'] = 1, ['F'] = 1, ['<'] = 1, ['='] = 1 };

static const oyl_schema failsafe_schema = { .rules = NULL, .rule_count = 0, DEFAULT_TAGS };

const oyl_schema *oyl_schema_failsafe(void) {
    return &failsafe_schema;
}

/* JSON: the spec's words, with Core's number rules */
static const oyl_schema_rule json_rules[] = {
    NULL_WORD("null"),
    TRUE_WORD("true"),
    FALSE_WORD("false"),
    INT_RULE(OYL_INT_CORE),
    FLOAT_RULE(OYL_FLOAT_CORE),
};

CAND(json_rules)
static const oyl_schema json_schema = PRESET(json_rules, json_first);

const oyl_schema *oyl_schema_json(void) {
    return &json_schema;
}

static const oyl_schema_rule core_rules[] = {
    NULL_WORD("null"), NULL_WORD("Null"), NULL_WORD("NULL"), NULL_WORD("~"), NULL_WORD(""),
    TRUE_WORD("true"), TRUE_WORD("True"), TRUE_WORD("TRUE"),
    FALSE_WORD("false"), FALSE_WORD("False"), FALSE_WORD("FALSE"),
    INT_RULE(OYL_INT_CORE),
    FLOAT_RULE(OYL_FLOAT_CORE),
};

CAND(core_rules)
static const oyl_schema core_schema = PRESET(core_rules, core_first);

const oyl_schema *oyl_schema_core(void) {
    return &core_schema;
}

/* YAML 1.1's types as PyYAML reads them, PyYAML being libyaml's usual
 * loader: unlike yaml.org/type, y and n are text, and so is a signed
 * leading-dot float (-.5); where the spec's patterns contradict
 * themselves, a float needs a digit (the float pattern would take "." and
 * "1.2.3"), and a zone may follow a space (as the timestamp type's own
 * example has it) */
static const oyl_schema_rule yaml11_rules[] = {
    NULL_WORD("~"), NULL_WORD("null"), NULL_WORD("Null"), NULL_WORD("NULL"), NULL_WORD(""),
    TRUE_WORD("yes"), TRUE_WORD("Yes"), TRUE_WORD("YES"),
    TRUE_WORD("true"), TRUE_WORD("True"), TRUE_WORD("TRUE"),
    TRUE_WORD("on"), TRUE_WORD("On"), TRUE_WORD("ON"),
    FALSE_WORD("no"), FALSE_WORD("No"), FALSE_WORD("NO"),
    FALSE_WORD("false"), FALSE_WORD("False"), FALSE_WORD("FALSE"),
    FALSE_WORD("off"), FALSE_WORD("Off"), FALSE_WORD("OFF"),
    WORD("<<", TAG(MERGE), V_TEXT),
    WORD("=", TAG(VALUE), V_TEXT),
    INT_RULE(OYL_INT_YAML11),
    FLOAT_RULE(OYL_FLOAT_YAML11),
    TIMESTAMP11_RULE,
};

CAND(yaml11_rules)
static const oyl_schema yaml11_schema = PRESET(yaml11_rules, yaml11_first);

const oyl_schema *oyl_schema_yaml11(void) {
    return &yaml11_schema;
}

/* go-yaml v2 (resolve.go, v2 branch): its word map, then for text that
 * starts with a digit or sign a timestamp, then Go's ParseInt(s, 0, 64)
 * and ParseUint with underscores removed, then a float. An int too big
 * for 64 bits falls to the float rule, which takes digits alone. */
#define GOYAML2_INT (OYL_INT_SIGN | OYL_INT_HEX | OYL_INT_OCT | OYL_INT_OCT_ZERO | \
                     OYL_INT_BIN | OYL_INT_PREFIX_CASE | OYL_INT_SIGN_PREFIX |     \
                     OYL_INT_UNDERSCORE | OYL_INT_RANGE)
#define GOYAML2_FLOAT (OYL_FLOAT_SPECIALS | OYL_FLOAT_DIGITS | OYL_FLOAT_UNDERSCORE | \
                       OYL_FLOAT_RANGE)

static const oyl_schema_rule goyaml2_rules[] = {
    NULL_WORD(""), NULL_WORD("~"), NULL_WORD("null"), NULL_WORD("Null"), NULL_WORD("NULL"),
    TRUE_WORD("y"), TRUE_WORD("Y"), TRUE_WORD("yes"), TRUE_WORD("Yes"), TRUE_WORD("YES"),
    TRUE_WORD("true"), TRUE_WORD("True"), TRUE_WORD("TRUE"),
    TRUE_WORD("on"), TRUE_WORD("On"), TRUE_WORD("ON"),
    FALSE_WORD("n"), FALSE_WORD("N"), FALSE_WORD("no"), FALSE_WORD("No"), FALSE_WORD("NO"),
    FALSE_WORD("false"), FALSE_WORD("False"), FALSE_WORD("FALSE"),
    FALSE_WORD("off"), FALSE_WORD("Off"), FALSE_WORD("OFF"),
    TIMESTAMP_RULE,
    INT_RULE(GOYAML2_INT),
    FLOAT_RULE(GOYAML2_FLOAT),
};

CAND(goyaml2_rules)
static const oyl_schema goyaml2_schema = PRESET(goyaml2_rules, goyaml2_first);

#if defined(__GNUC__) || defined(__clang__)
#  define FILL(r) fill_cand(r, (int)(sizeof(r) / sizeof(r[0])), r##_at, r##_cand, sizeof r##_cand)
__attribute__((constructor)) static void fill_preset_cands(void) {
    FILL(json_rules);
    FILL(core_rules);
    FILL(yaml11_rules);
    FILL(goyaml2_rules);
}
#endif

const oyl_schema *oyl_schema_goyaml2(void) {
    return &goyaml2_schema;
}

/* ── Schema builder ──────────────────────────────────────── */

struct oyl_schema_builder {
    oyl_arena       *arena;
    oyl_schema_rule *rules;
    int              len;
    int              cap;
    bool             oom;   /* an add failed; finish() returns NULL */
};

oyl_schema_builder *oyl_schema_builder_new(oyl_arena *a) {
    oyl_schema_builder *b = malloc(sizeof(*b));
    if (!b) return NULL;
    b->arena = a;
    b->cap = 16;
    b->len = 0;
    b->oom = false;
    b->rules = malloc(b->cap * sizeof(oyl_schema_rule));
    if (!b->rules) { free(b); return NULL; }
    return b;
}

/* Make room for `need` more rules. On failure the builder is marked and
 * the caller must not add. */
static bool builder_ensure(oyl_schema_builder *b, int need) {
    if (b->oom || need < 0) return false;
    if (b->len + need <= b->cap) return true;
    int new_cap = b->cap * 2;
    while (new_cap < b->len + need) new_cap *= 2;
    oyl_schema_rule *new_rules = realloc(b->rules, (size_t)new_cap * sizeof(oyl_schema_rule));
    if (!new_rules) { b->oom = true; return false; }
    b->rules = new_rules;
    b->cap = new_cap;
    return true;
}

static void builder_push(oyl_schema_builder *b, oyl_schema_rule r) {
    if (builder_ensure(b, 1)) b->rules[b->len++] = r;
}

/* A word added with oyl_schema_builder_add: null and bool tags get those
 * values (false for false, no, n and off in any case), others the text */
static oyl_value word_value(const char *pattern, oyl_str tag) {
    if (str_eq(tag, OYL_TAG_NULL)) return (oyl_value)V_NULL;
    if (str_eq(tag, OYL_TAG_BOOL)) {
        static const char *falses[] = { "false", "no", "n", "off" };
        oyl_str w = { pattern, strlen(pattern) };
        for (size_t i = 0; i < sizeof falses / sizeof falses[0]; i++)
            if (word_eq(falses[i], strlen(falses[i]), w, true)) return (oyl_value)V_FALSE;
        return (oyl_value)V_TRUE;
    }
    return (oyl_value){ .kind = OYL_VALUE_STR };
}

void oyl_schema_builder_add(oyl_schema_builder *b,
                            oyl_match_type match,
                            const char *pattern, oyl_str tag) {
    oyl_schema_rule r = { .kind = RULE_NONE, .pattern = pattern, .plen = strlen(pattern), .tag = tag };
    switch (match) {
    case OYL_MATCH_EXACT:
    case OYL_MATCH_ICASE:
        r.kind = match == OYL_MATCH_EXACT ? RULE_WORD : RULE_WORD_ICASE;
        r.value = word_value(pattern, tag);
        break;
    case OYL_MATCH_BUILTIN:
        /* the 1.0 matchers: Core's ints, and floats with a dot or exponent */
        if (strcmp(pattern, "int") == 0) {
            r.kind = RULE_INT;
            r.flags = OYL_INT_CORE;
        } else if (strcmp(pattern, "float") == 0) {
            r.kind = RULE_FLOAT;
            r.flags = OYL_FLOAT_SPECIALS;
        }
        break;
    }
    builder_push(b, r);
}

void oyl_schema_builder_add_bools(oyl_schema_builder *b,
                                  const char **true_terms, int ntrue,
                                  const char **false_terms, int nfalse) {
    if (!builder_ensure(b, ntrue + nfalse)) return;
    for (int i = 0; i < ntrue; i++)
        b->rules[b->len++] = (oyl_schema_rule){ .kind = RULE_WORD, .pattern = true_terms[i],
                                                .plen = strlen(true_terms[i]),
                                                .tag = OYL_TAG_BOOL, .value = V_TRUE };
    for (int i = 0; i < nfalse; i++)
        b->rules[b->len++] = (oyl_schema_rule){ .kind = RULE_WORD, .pattern = false_terms[i],
                                                .plen = strlen(false_terms[i]),
                                                .tag = OYL_TAG_BOOL, .value = V_FALSE };
}

void oyl_schema_builder_add_nulls(oyl_schema_builder *b,
                                  const char **terms, int nterms) {
    if (!builder_ensure(b, nterms)) return;
    for (int i = 0; i < nterms; i++)
        b->rules[b->len++] = (oyl_schema_rule){ .kind = RULE_WORD, .pattern = terms[i],
                                                .plen = strlen(terms[i]),
                                                .tag = OYL_TAG_NULL, .value = V_NULL };
}

void oyl_schema_builder_add_int(oyl_schema_builder *b) {
    oyl_schema_builder_add(b, OYL_MATCH_BUILTIN, "int", OYL_TAG_INT);
}

void oyl_schema_builder_add_float(oyl_schema_builder *b) {
    oyl_schema_builder_add(b, OYL_MATCH_BUILTIN, "float", OYL_TAG_FLOAT);
}

void oyl_schema_builder_add_int_flags(oyl_schema_builder *b, unsigned flags) {
    builder_push(b, (oyl_schema_rule){ .kind = RULE_INT, .tag = OYL_TAG_INT, .flags = flags });
}

void oyl_schema_builder_add_float_flags(oyl_schema_builder *b, unsigned flags) {
    builder_push(b, (oyl_schema_rule){ .kind = RULE_FLOAT, .tag = OYL_TAG_FLOAT, .flags = flags });
}

void oyl_schema_builder_add_type(oyl_schema_builder *b, oyl_str tag,
                                 oyl_scalar_parser parse, void *user) {
    builder_push(b, (oyl_schema_rule){ .kind = parse ? RULE_CUSTOM : RULE_NONE, .tag = tag,
                                       .parse = parse, .user = user });
}

const oyl_schema *oyl_schema_builder_finish(oyl_schema_builder *b) {
    if (b->oom) return NULL;
    /* Copy the schema, its rules, and their patterns and tags into the
     * arena, so it outlives the builder and the caller's strings */
    oyl_schema *schema = oyl_arena_alloc(b->arena, sizeof *schema, _Alignof(oyl_schema));
    oyl_schema_rule *stable = oyl_arena_alloc(
        b->arena, (size_t)b->len * sizeof(oyl_schema_rule) + 1, _Alignof(oyl_schema_rule));
    if (!schema || !stable) return NULL;
    for (int i = 0; i < b->len; i++) {
        oyl_schema_rule r = b->rules[i];
        if (r.pattern) {
            r.pattern = oyl_arena_dup(b->arena, r.pattern, r.plen);
            if (!r.pattern) return NULL;
        }
        if (r.tag.data) {
            r.tag.data = oyl_arena_dup(b->arena, r.tag.data, r.tag.len);
            if (!r.tag.data) return NULL;
        }
        stable[i] = r;
    }
    /* the first bytes its rules can accept; a custom parser could take any */
    bool *first = oyl_arena_alloc(b->arena, 256 * sizeof *first, _Alignof(bool));
    if (!first) return NULL;
    memset(first, 0, 256 * sizeof *first);
    for (int i = 0; i < b->len; i++) {
        const oyl_schema_rule *r = &stable[i];
        switch (r->kind) {
        case RULE_WORD:
        case RULE_WORD_ICASE:
            if (r->plen == 0) break;   /* the empty word needs no first byte */
            first[(unsigned char)r->pattern[0]] = true;
            if (r->kind == RULE_WORD_ICASE) {
                first[(unsigned char)tolower((unsigned char)r->pattern[0])] = true;
                first[(unsigned char)toupper((unsigned char)r->pattern[0])] = true;
            }
            break;
        case RULE_INT:
        case RULE_FLOAT:
        case RULE_TIMESTAMP:
            for (int c = '0'; c <= '9'; c++) first[c] = true;
            first['+'] = first['-'] = first['.'] = true;
            break;
        case RULE_CUSTOM:
            memset(first, 1, 256 * sizeof *first);
            break;
        case RULE_NONE:
            break;
        }
    }
    *schema = (oyl_schema){ .rules = stable, .rule_count = b->len, DEFAULT_TAGS, .first = first };
    long ncand = fill_cand(stable, b->len, NULL, NULL, 0);
    if (ncand > 0) {
        uint16_t *at = oyl_arena_alloc(b->arena, 258 * sizeof *at, _Alignof(uint16_t));
        uint8_t *cand = oyl_arena_alloc(b->arena, (size_t)ncand, 1);
        if (at && cand && fill_cand(stable, b->len, at, cand, (size_t)ncand) == ncand) {
            schema->cand_at = at;
            schema->cand = cand;
        }
    }
    return schema;
}

void oyl_schema_builder_free(oyl_schema_builder *b) {
    if (!b) return;
    free(b->rules);
    free(b);
}
