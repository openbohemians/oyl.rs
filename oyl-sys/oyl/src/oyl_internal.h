/*
 * oyl_internal.h — Declarations shared between oyl's source files.
 *
 * Not installed. Everything here is hidden from the shared library's
 * exported symbols (the library is built with -fvisibility=hidden, and
 * only functions marked OYL_API in oyl.h are exported), so it can change
 * freely without affecting the ABI.
 */

#ifndef OYL_INTERNAL_H
#define OYL_INTERNAL_H

#include "oyl/oyl.h"
#include <string.h>

/* ── Schema ──────────────────────────────────────────────── */

/* How a rule parses a scalar */
typedef enum {
    RULE_WORD,         /* the text is `pattern`; the value is `value` */
    RULE_WORD_ICASE,   /* the same, ignoring ASCII case */
    RULE_INT,          /* the built-in int parser, with OYL_INT_ `flags` */
    RULE_FLOAT,        /* the built-in float parser, with OYL_FLOAT_ `flags` */
    RULE_TIMESTAMP,    /* go-yaml v2's timestamps; the value is the text */
    RULE_CUSTOM,       /* `parse(text, out, user)` */
    RULE_NONE,         /* never accepts (an unknown builtin name) */
} oyl_rule_kind;

/* A single tag resolution rule: a plain scalar the rule's parser accepts
 * resolves to `tag`, and the parser gives its value. */
typedef struct {
    oyl_rule_kind     kind;
    const char       *pattern;   /* the word, for RULE_WORD and RULE_WORD_ICASE */
    size_t            plen;      /* its length */
    oyl_str           tag;
    unsigned          flags;
    oyl_value         value;     /* a word's value (STR means the text) */
    oyl_scalar_parser parse;
    void             *user;
} oyl_schema_rule;

struct oyl_schema {
    const oyl_schema_rule *rules;
    int                    rule_count;
    oyl_str                default_plain_tag;   /* unmatched plain scalars */
    oyl_str                default_quoted_tag;  /* all quoted scalars */
    oyl_str                default_seq_tag;     /* untagged sequences */
    oyl_str                default_map_tag;     /* untagged mappings */
    /* the first bytes any rule could accept: a plain scalar starting with
     * another byte is the default at once (NULL: no shortcut) */
    const bool            *first;
    /* by first byte c, the rules that may accept the text, in order:
     * cand[cand_at[c]] up to cand[cand_at[c + 1]], with c 256 for empty
     * text. Every rule is tried when cand_at is NULL or cand_at[257] 0. */
    const uint16_t        *cand_at;
    const uint8_t         *cand;
};

/* ── Dialects and warnings ───────────────────────────────── */

struct oyl_dialect {
    const oyl_schema *schema;
    unsigned          syntax;   /* OYL_SYNTAX_ */
    oyl_merge_order   merge;
};

/* Where warnings go. The parser re-reads input when it falls back to its
 * eager mode, so a warning is reported only past the last one of its code. */
typedef struct {
    oyl_warn_fn fn;
    void       *user;
    size_t      last[8];   /* per code: the offset of the last one reported */
    bool        seen[8];
} oyl_warn_state;

/* The rule that resolves untagged plain `text`, or -1 for the default (for
 * oyl_tree.c, which keeps it per node) */
int oyl_schema_rule_of(const oyl_schema *schema, oyl_str text);
/* Rule `rule`'s value of `text`: 1 accepted, 0 declined, -1 out of memory */
int oyl_schema_rule_value(const oyl_schema *schema, int rule, oyl_str text, oyl_value *out);

/* A scalar as mapping keys are compared: two are equal when their tags
 * and values are (YAML 1.2.2 §3.2.1.3), so 0x1 and 1 are one key, 1 and
 * "1" two. Inline: trees sort keys with these. */
typedef struct {
    oyl_str   tag;
    oyl_value value;
} oyl_key;

/* A key's tag from the scalar's (written, else resolved): ! makes it a
 * string */
static inline oyl_str oyl_key_tag(const oyl_schema *schema, oyl_str tag) {
    return tag.len == 1 && tag.data[0] == '!' ? schema->default_quoted_tag : tag;
}

/* The key for a scalar with tag `tag` (from oyl_key_tag) and `text`, given
 * the status and value oyl_schema_value_of gave it: one its tag rejects is
 * its text */
static inline void oyl_key_make(oyl_key *k, oyl_str tag, oyl_str text, oyl_status st,
                                const oyl_value *v) {
    k->tag = tag;
    if (st == OYL_OK) k->value = *v;
    else k->value = (oyl_value){ .kind = OYL_VALUE_STR, .as = { .s = text } };
    /* one int, whichever kind a parser gave it */
    if (k->value.kind == OYL_VALUE_UINT && k->value.as.u <= INT64_MAX)
        k->value = (oyl_value){ .kind = OYL_VALUE_INT, .as = { .i = (int64_t)k->value.as.u } };
}

static inline int oyl_bytes_cmp(oyl_str a, oyl_str b) {
    size_t n = a.len < b.len ? a.len : b.len;
    int r = n ? memcmp(a.data, b.data, n) : 0;
    return r ? r : (a.len > b.len) - (a.len < b.len);
}

/* Are two keys' tags the same? */
static inline bool oyl_key_tag_eq(oyl_str a, oyl_str b) {
    return (a.data == b.data && a.len == b.len) ||
           (a.len == b.len && (a.len == 0 || memcmp(a.data, b.data, a.len) == 0));
}

/* Order keys, 0 when equal: by tag, then value, NaN equal to NaN */
static inline int oyl_key_cmp(const oyl_key *a, const oyl_key *b) {
    if (!oyl_key_tag_eq(a->tag, b->tag)) return oyl_bytes_cmp(a->tag, b->tag);
    const oyl_value *x = &a->value, *y = &b->value;
    if (x->kind != y->kind) return x->kind < y->kind ? -1 : 1;
    switch (x->kind) {
    case OYL_VALUE_NULL:  return 0;
    case OYL_VALUE_BOOL:  return (x->as.b > y->as.b) - (x->as.b < y->as.b);
    case OYL_VALUE_INT:   return (x->as.i > y->as.i) - (x->as.i < y->as.i);
    case OYL_VALUE_UINT:  return (x->as.u > y->as.u) - (x->as.u < y->as.u);
    case OYL_VALUE_FLOAT: {
        /* .nan's canonical form is one, so NaN equals NaN, after the numbers */
        bool nx = x->as.f != x->as.f, ny = y->as.f != y->as.f;
        if (nx || ny) return nx - ny;
        return (x->as.f > y->as.f) - (x->as.f < y->as.f);
    }
    default:              return oyl_bytes_cmp(x->as.s, y->as.s);   /* text */
    }
}

/* Report a warning from parsing, once (see oyl_warn_state) */
void oyl_warn(oyl_warn_state *w, oyl_warning code, oyl_mark mark, const char *msg);

/* ── Scanner ─────────────────────────────────────────────── */

/* Scan the next token into caller-provided storage. oyl_scan_next() wraps
 * this for the public API; the parser calls it directly. */
oyl_status oyl_scan_token(oyl_scanner *s, oyl_token *tok);
/* The syntax leniencies to apply (OYL_SYNTAX_), and where to warn of their
 * use; the parser sets them per document. */
void oyl_scanner_set_syntax(oyl_scanner *s, unsigned syntax, oyl_warn_state *w);
/* Copy one scanner's state into another over the same input (the parser's
 * checkpoint at a document start). */
bool oyl_scanner_copy(oyl_scanner *dst, const oyl_scanner *src);

/* ── Parser, for oyl_json.c and oyl_tree.c ──────────────────────────────── */

/* Report an error found after parsing (a converter's), as parse errors
 * are: oyl_parser_error() and oyl_parser_error_mark() return it. */
void oyl_parser_fail(oyl_parser *p, oyl_mark mark, const char *msg);
oyl_arena        *oyl_parser_arena(const oyl_parser *p);
const char       *oyl_parser_input(const oyl_parser *p, size_t *len);
/* Whether `d` is a dialect the parser made itself (from a schema alone),
 * which lives only as long as the parser */
bool              oyl_parser_owns_dialect(const oyl_parser *p, const oyl_dialect *d);
int               oyl_parser_depth_limit(const oyl_parser *p);   /* 0: none */
const oyl_schema *oyl_parser_schema(const oyl_parser *p);       /* NULL: none */
/* Report a warning found after parsing (no re-reading to guard against) */
void              oyl_parser_warn(oyl_parser *p, oyl_warning code, oyl_mark mark,
                                  const char *msg);

/* ── Trees, for oyl_json.c ───────────────────────────────── */

struct oyl_tree {
    const char        *input;       /* the whole input */
    size_t             input_len;
    size_t             base;        /* where the document starts in it */
    oyl_node          *nodes;
    uint32_t           len, cap;
    oyl_node_props    *props;       /* props[0] is unused: 0 means none */
    uint32_t           plen, pcap;
    char              *pool;
    size_t             pool_len, pool_cap;
    oyl_mark          *marks;       /* marks[k]: node k * MARK_EVERY's start */
    uint32_t           mlen, mcap;
    oyl_mark           doc_mark;    /* the document's start */
    const oyl_dialect *dialect;
    const oyl_schema  *schema;
    oyl_merge_order    merge;
    bool               implicit_start, implicit_end;   /* no "---", no "..." */
    oyl_dialect        own_dialect;   /* a copy of a dialect the parser owned */
};

/* A scalar's or alias's text */
static inline oyl_str oyl_tree_text(const oyl_tree *t, const oyl_node *n) {
    if (!n->len) return (oyl_str){ "", 0 };
    return n->flags & OYL_NODE_POOLED ? (oyl_str){ t->pool + n->text, n->len }
                                      : (oyl_str){ t->input + t->base + n->text, n->len };
}

/* For small helpers on the per-token path that GCC may otherwise decline
 * to inline once they grow a little. */
#if defined(__GNUC__) || defined(__clang__)
#  define ALWAYS_INLINE static inline __attribute__((always_inline))
#else
#  define ALWAYS_INLINE static inline
#endif

/* For cold paths that would otherwise be inlined into a hot caller. */
#if defined(__GNUC__) || defined(__clang__)
#  define NOINLINE static __attribute__((noinline))
#else
#  define NOINLINE static
#endif

#endif /* OYL_INTERNAL_H */
