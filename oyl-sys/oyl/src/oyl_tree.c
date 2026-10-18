/*
 * oyl_tree.c — documents in memory (oyl_parse_tree)
 *
 * A tree is a document's nodes in document order, each collection with
 * the size of its subtree instead of links to its children (oyl.h,
 * "Trees"). It is built by appending the parser's events. Positions are
 * 32-bit offsets from the document's start, and text the parser rewrote
 * is copied into the tree's pool, so a tree holds no pointers: a binding
 * can copy it whole, and parts built separately can be joined.
 */

#include "oyl_internal.h"
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define NONE OYL_NO_NODE

/* A mark is kept for every MARK_EVERY-th node; oyl_node_mark counts lines
 * from the nearest one */
#define MARK_EVERY 64

/* struct oyl_tree is in oyl_internal.h, for oyl_json.c */

/* How deep merges may nest (a merge's source with merges of its own) */
#define MAX_MERGE_DEPTH 256

static bool same(oyl_str a, oyl_str b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.data, b.data, a.len) == 0);
}

/* Make room for `need` more elements in a growable array */
NOINLINE bool grow_slow(void **data, uint32_t *cap, uint32_t len, uint32_t need, size_t size) {
    uint64_t want = (uint64_t)len + need, c = *cap ? *cap : 64;
    while (c < want) c *= 2;
    if (c > UINT32_MAX) c = UINT32_MAX;
    if (c < want || c > SIZE_MAX / size) return false;
    void *p = realloc(*data, (size_t)c * size);
    if (!p) return false;
    *data = p;
    *cap = (uint32_t)c;
    return true;
}

static inline bool grow(void **data, uint32_t *cap, uint32_t len, uint32_t need, size_t size) {
    return need <= *cap - len || grow_slow(data, cap, len, need, size);
}

/* ── Building ────────────────────────────────────────────── */

typedef struct {
    oyl_str  name;
    uint32_t node;
} anchor;

typedef struct {
    oyl_parser       *p;
    oyl_tree         *t;
    struct open { uint32_t node, last, count; } *stack;   /* open collections */
    uint32_t          slen, scap;
    anchor           *anchors;      /* open addressing; name.data NULL is empty */
    size_t            acap, alen;
    const oyl_schema *parser_schema;   /* a parser resolving tags: its tags aren't kept */
    uintptr_t         lo, hi;       /* the document's text: from its start to the input's end */
    size_t            base;
    bool              small;        /* under 4 GB from the document's start: no offset overflows */
    oyl_str           last_tag;     /* the last tag pooled, and where */
    uint32_t          last_tag_at;
    oyl_status        status;
} builder;

static bool fail(builder *b, oyl_status status, oyl_mark mark, const char *fmt, ...) {
    if (b->status == OYL_OK) {
        char msg[256];
        va_list ap;
        va_start(ap, fmt);
        vsnprintf(msg, sizeof msg, fmt, ap);
        va_end(ap);
        oyl_parser_fail(b->p, mark, msg);
        b->status = status;
    }
    return false;
}

static bool out_of_memory(builder *b) {
    return fail(b, OYL_ERR_MEMORY, (oyl_mark){ 0, 0, 0 }, "out of memory");
}

static size_t name_hash(oyl_str s) {
    uint64_t h = 14695981039346656037u;
    for (size_t i = 0; i < s.len; i++) h = (h ^ (unsigned char)s.data[i]) * 1099511628211u;
    return (size_t)h;
}

static anchor *anchor_slot(builder *b, oyl_str name) {
    size_t i = name_hash(name) & (b->acap - 1);
    while (b->anchors[i].name.data && !same(b->anchors[i].name, name)) i = (i + 1) & (b->acap - 1);
    return &b->anchors[i];
}

/* Bind `name` to node `n`; a later anchor of the same name replaces it, so
 * an alias refers to the closest one before it */
static bool anchor_set(builder *b, oyl_str name, uint32_t n) {
    if (2 * (b->alen + 1) > b->acap) {
        size_t cap = b->acap ? b->acap * 2 : 64;
        anchor *old = b->anchors;
        size_t old_cap = b->acap;
        b->anchors = calloc(cap, sizeof *b->anchors);
        if (!b->anchors) { b->anchors = old; return out_of_memory(b); }
        b->acap = cap;
        for (size_t i = 0; i < old_cap; i++)
            if (old[i].name.data) *anchor_slot(b, old[i].name) = old[i];
        free(old);
    }
    anchor *a = anchor_slot(b, name);
    if (!a->name.data) b->alen++;
    *a = (anchor){ name, n };
    return true;
}

static uint32_t anchor_get(builder *b, oyl_str name) {
    if (!b->acap) return NONE;
    anchor *a = anchor_slot(b, name);
    return a->name.data ? a->node : NONE;
}

/* An input offset as a position in the document */
static inline bool position(builder *b, const oyl_event *e, size_t offset, uint32_t *out) {
    if (b->small) {   /* nodes don't start before their document */
        *out = (uint32_t)(offset - b->base);
        return true;
    }
    size_t rel = offset >= b->base ? offset - b->base : 0;
    if (rel > UINT32_MAX)
        return fail(b, OYL_ERR_LIMIT, e->start, "document too large for a tree (over 4 GB)");
    *out = (uint32_t)rel;
    return true;
}

static bool pool_add(builder *b, oyl_str s, uint32_t *at) {
    oyl_tree *t = b->t;
    if (s.len > UINT32_MAX - t->pool_len)
        return fail(b, OYL_ERR_LIMIT, (oyl_mark){ 0, 0, 0 }, "document too large for a tree (over 4 GB)");
    if (s.len > t->pool_cap - t->pool_len) {
        size_t cap = t->pool_cap ? t->pool_cap : 256;
        while (cap - t->pool_len < s.len) cap *= 2;
        char *pool = realloc(t->pool, cap);
        if (!pool) return out_of_memory(b);
        t->pool = pool;
        t->pool_cap = cap;
    }
    if (s.len) memcpy(t->pool + t->pool_len, s.data, s.len);
    *at = (uint32_t)t->pool_len;
    t->pool_len += s.len;
    return true;
}

/* Where text is: in the document, or (rewritten by the parser) copied into
 * the pool */
static inline bool place_text(builder *b, oyl_str s, oyl_node *n) {
    n->len = (uint32_t)s.len;
    if (s.len == 0) { n->text = 0; return true; }
    uintptr_t p = (uintptr_t)s.data;
    if (p >= b->lo && p <= b->hi && s.len <= b->hi - p && (b->small || p - b->lo <= UINT32_MAX)) {
        n->text = (uint32_t)(p - b->lo);
        return true;
    }
    if (s.len > UINT32_MAX)
        return fail(b, OYL_ERR_LIMIT, (oyl_mark){ 0, 0, 0 }, "scalar too large for a tree (over 4 GB)");
    n->flags |= OYL_NODE_POOLED;
    return pool_add(b, s, &n->text);
}

/* A tag into the pool; nodes often repeat the last one */
static bool place_tag(builder *b, oyl_str tag, uint32_t *at) {
    if (b->last_tag.len && same(tag, b->last_tag)) {
        *at = b->last_tag_at;
        return true;
    }
    if (!pool_add(b, tag, at)) return false;
    b->last_tag = tag;
    b->last_tag_at = *at;
    return true;
}

static bool add_props(builder *b, oyl_str tag, oyl_str anchor_name, oyl_node *n) {
    oyl_tree *t = b->t;
    if (!grow((void **)&t->props, &t->pcap, t->plen, 1, sizeof *t->props)) return out_of_memory(b);
    oyl_node_props pr = { 0, 0, 0, 0 };
    if (tag.len) {
        if (!place_tag(b, tag, &pr.tag)) return false;
        pr.tag_len = (uint32_t)tag.len;
    }
    if (anchor_name.len) {
        if (!pool_add(b, anchor_name, &pr.anchor)) return false;
        pr.anchor_len = (uint32_t)anchor_name.len;
    }
    n->props = t->plen;
    t->props[t->plen++] = pr;
    return true;
}

/* The tag a node keeps: the one written. A parser with a schema tags
 * every node; a tag it would resolve anyway isn't kept. */
static oyl_str written_tag(builder *b, const oyl_event *e) {
    const oyl_schema *s = b->parser_schema;
    if (!s || !e->tag.len) return e->tag;
    oyl_str resolved = e->type == OYL_EVT_SCALAR ? oyl_schema_resolve(s, e->value, e->scalar_style)
                     : e->type == OYL_EVT_MAPPING_START ? s->default_map_tag : s->default_seq_tag;
    return same(e->tag, resolved) ? OYL_STR_NULL : e->tag;
}

static bool close_collection(builder *b, const oyl_event *e) {
    oyl_tree *t = b->t;
    if (!b->slen) return true;
    struct open o = b->stack[--b->slen];
    oyl_node *n = &t->nodes[o.node];
    n->span = t->len - o.node;
    n->len = n->kind == OYL_NODE_MAPPING ? o.count / 2 : o.count;
    /* a block collection ends where its last child does (its end event
     * comes at the next token, past comments and blank lines) */
    if ((n->flags & OYL_NODE_FLOW) || o.last == NONE) return position(b, e, e->end.offset, &n->end);
    n->end = t->nodes[o.last].end;
    return true;
}

ALWAYS_INLINE bool add_event(builder *b, const oyl_event *e) {
    oyl_tree *t = b->t;
    oyl_node n = { 0 };
    switch (e->type) {
    case OYL_EVT_SCALAR:         n.kind = OYL_NODE_SCALAR; break;
    case OYL_EVT_ALIAS:          n.kind = OYL_NODE_ALIAS; break;
    case OYL_EVT_SEQUENCE_START: n.kind = OYL_NODE_SEQUENCE; break;
    case OYL_EVT_MAPPING_START:  n.kind = OYL_NODE_MAPPING; break;
    case OYL_EVT_SEQUENCE_END:
    case OYL_EVT_MAPPING_END:    return close_collection(b, e);
    default:                     return true;
    }
    if (t->len >= UINT32_MAX - 1)
        return fail(b, OYL_ERR_LIMIT, e->start, "document too large for a tree (over 4 billion nodes)");
    uint32_t i = t->len;
    if (b->slen == 0 && i != 0) return true;   /* a document has one root */
    if (!grow((void **)&t->nodes, &t->cap, t->len, 1, sizeof *t->nodes)) return out_of_memory(b);
    if (i % MARK_EVERY == 0) {
        if (!grow((void **)&t->marks, &t->mcap, t->mlen, 1, sizeof *t->marks)) return out_of_memory(b);
        t->marks[t->mlen++] = e->start;
    }
    if (!position(b, e, e->start.offset, &n.start)) return false;
    oyl_str tag = OYL_STR_NULL;
    if (n.kind == OYL_NODE_ALIAS) {
        n.props = anchor_get(b, e->value);
        if (n.props == NONE)
            return fail(b, OYL_ERR_PARSE, e->start, "unknown anchor '%.*s' referenced",
                        (int)e->value.len, e->value.data);
    } else {
        tag = written_tag(b, e);
    }
    if (n.kind == OYL_NODE_SCALAR || n.kind == OYL_NODE_ALIAS) {
        n.span = 1;
        n.style = (uint8_t)e->scalar_style;
        if (!place_text(b, e->value, &n) || !position(b, e, e->end.offset, &n.end)) return false;
        if (n.kind == OYL_NODE_SCALAR && !tag.len && e->scalar_style == OYL_SCALAR_PLAIN) {
            int r = oyl_schema_rule_of(t->schema, e->value);
            n.type = (uint8_t)(r < 0 ? 0 : r < 254 ? r + 1 : 255);
        }
    } else if (e->flow) {
        n.flags |= OYL_NODE_FLOW;
    }
    if ((tag.len || e->anchor.len) && !add_props(b, tag, e->anchor, &n)) return false;
    if (b->slen) {
        struct open *o = &b->stack[b->slen - 1];
        n.parent = i - o->node;
        if (t->nodes[o->node].kind == OYL_NODE_MAPPING && o->count % 2 == 0) n.flags |= OYL_NODE_KEY;
        o->count++;
        o->last = i;
    }
    t->nodes[t->len++] = n;
    if (e->anchor.len && !anchor_set(b, e->anchor, i)) return false;
    if (n.kind == OYL_NODE_SEQUENCE || n.kind == OYL_NODE_MAPPING) {
        if (!grow((void **)&b->stack, &b->scap, b->slen, 1, sizeof *b->stack)) return out_of_memory(b);
        b->stack[b->slen++] = (struct open){ i, NONE, 0 };
    }
    return true;
}

oyl_status oyl_parse_tree(oyl_parser *p, oyl_tree **tree) {
    if (!p || !tree) return OYL_ERR_INPUT;
    *tree = NULL;
    const oyl_event *e;
    oyl_status st;
    for (;;) {
        st = oyl_parse_next(p, &e);
        if (st != OYL_OK) return st;
        if (e->type == OYL_EVT_STREAM_START) continue;
        if (e->type == OYL_EVT_STREAM_END || e->type == OYL_EVT_NONE) return OYL_OK;
        if (e->type == OYL_EVT_DOC_START) break;
        oyl_parser_fail(p, e->start, "oyl_parse_tree: not at the start of a document");
        return OYL_ERR_INPUT;
    }
    oyl_tree *t = calloc(1, sizeof *t);
    if (!t) return OYL_ERR_MEMORY;
    t->input = oyl_parser_input(p, &t->input_len);
    t->base = e->start.offset;
    t->doc_mark = e->start;
    t->dialect = oyl_parser_document_dialect(p);
    if (oyl_parser_owns_dialect(p, t->dialect)) {   /* the tree may outlive the parser */
        t->own_dialect = *t->dialect;
        t->dialect = &t->own_dialect;
    }
    t->schema = oyl_dialect_schema(t->dialect);
    t->merge = oyl_dialect_merge(t->dialect);
    t->implicit_start = e->implicit;
    t->plen = 1;
    builder b = { .p = p, .t = t, .parser_schema = oyl_parser_schema(p), .base = t->base,
                  .lo = (uintptr_t)t->input + t->base, .hi = (uintptr_t)t->input + t->input_len,
                  .small = t->input_len - t->base <= UINT32_MAX };
    if (!grow((void **)&t->props, &t->pcap, 0, 1, sizeof *t->props)) {
        oyl_tree_free(t);
        return OYL_ERR_MEMORY;
    }
    t->props[0] = (oyl_node_props){ 0, 0, 0, 0 };
    while ((st = oyl_parse_next(p, &e)) == OYL_OK && e->type != OYL_EVT_DOC_END)
        if (!add_event(&b, e)) break;
    if (st == OYL_OK) t->implicit_end = e->implicit;
    if (st == OYL_OK && b.status == OYL_OK && t->len == 0) {
        /* no node: the document is an empty scalar */
        oyl_event empty = { .type = OYL_EVT_SCALAR, .start = t->doc_mark, .end = t->doc_mark };
        add_event(&b, &empty);
    }
    free(b.stack);
    free(b.anchors);
    if (st == OYL_OK) st = b.status;
    if (st != OYL_OK) {
        oyl_tree_free(t);
        return st;
    }
    *tree = t;
    return OYL_OK;
}

void oyl_tree_free(oyl_tree *t) {
    if (!t) return;
    free(t->nodes);
    free(t->props);
    free(t->pool);
    free(t->marks);
    free(t);
}

/* ── Reading ─────────────────────────────────────────────── */

const oyl_node *oyl_tree_nodes(const oyl_tree *t, uint32_t *count) {
    if (count) *count = t ? t->len : 0;
    return t ? t->nodes : NULL;
}

const oyl_node_props *oyl_tree_props(const oyl_tree *t, uint32_t *count) {
    if (count) *count = t ? t->plen : 0;
    return t ? t->props : NULL;
}

oyl_str oyl_tree_pool(const oyl_tree *t) {
    return t && t->pool_len ? (oyl_str){ t->pool, t->pool_len } : (oyl_str){ "", 0 };
}

size_t oyl_tree_base(const oyl_tree *t) { return t ? t->base : 0; }

const oyl_dialect *oyl_tree_dialect(const oyl_tree *t) { return t ? t->dialect : NULL; }

static oyl_str pooled(const oyl_tree *t, uint32_t at, uint32_t len) {
    return len ? (oyl_str){ t->pool + at, len } : OYL_STR_NULL;
}


oyl_str oyl_node_text(const oyl_tree *t, uint32_t i) {
    if (!t || i >= t->len) return OYL_STR_NULL;
    const oyl_node *n = &t->nodes[i];
    if (n->kind != OYL_NODE_SCALAR && n->kind != OYL_NODE_ALIAS) return OYL_STR_NULL;
    return oyl_tree_text(t, n);
}

static oyl_str written(const oyl_tree *t, const oyl_node *n) {
    if (n->kind == OYL_NODE_ALIAS || !n->props) return OYL_STR_NULL;
    return pooled(t, t->props[n->props].tag, t->props[n->props].tag_len);
}

oyl_str oyl_node_tag(const oyl_tree *t, uint32_t i) {
    if (!t || i >= t->len) return OYL_STR_NULL;
    const oyl_node *n = &t->nodes[i];
    if (n->kind == OYL_NODE_ALIAS) return OYL_STR_NULL;
    oyl_str tag = written(t, n);
    if (tag.len) return tag;
    const oyl_schema *s = t->schema;
    switch (n->kind) {
    case OYL_NODE_SEQUENCE: return s->default_seq_tag;
    case OYL_NODE_MAPPING:  return s->default_map_tag;
    default: break;
    }
    if (n->style != OYL_SCALAR_PLAIN) return s->default_quoted_tag;
    if (n->type == 0) return s->default_plain_tag;
    if (n->type == 255) return oyl_schema_resolve(s, oyl_tree_text(t, n), OYL_SCALAR_PLAIN);
    return s->rules[n->type - 1].tag;
}

oyl_str oyl_node_anchor(const oyl_tree *t, uint32_t i) {
    if (!t || i >= t->len) return OYL_STR_NULL;
    const oyl_node *n = &t->nodes[i];
    if (n->kind == OYL_NODE_ALIAS || !n->props) return OYL_STR_NULL;
    return pooled(t, t->props[n->props].anchor, t->props[n->props].anchor_len);
}

uint32_t oyl_node_target(const oyl_tree *t, uint32_t i) {
    if (!t || i >= t->len) return NONE;
    /* an alias can't have an anchor, so its target is never an alias */
    return t->nodes[i].kind == OYL_NODE_ALIAS ? t->nodes[i].props : i;
}

oyl_status oyl_node_value(const oyl_tree *t, uint32_t i, oyl_value *v) {
    if (!t || !v || i >= t->len) return OYL_ERR_INPUT;
    i = oyl_node_target(t, i);
    const oyl_node *n = &t->nodes[i];
    if (n->kind != OYL_NODE_SCALAR) return OYL_ERR_INPUT;
    oyl_str text = oyl_tree_text(t, n), tag = written(t, n);
    if (tag.len || n->type == 255)
        return oyl_schema_value_of(t->schema, text, (oyl_scalar_style)n->style, tag, v);
    if (n->type == 0) {
        *v = (oyl_value){ .kind = OYL_VALUE_STR, .as = { .s = text } };
        return OYL_OK;
    }
    int r = oyl_schema_rule_value(t->schema, n->type - 1, text, v);
    if (r < 0) return OYL_ERR_MEMORY;
    if (r == 0) return oyl_schema_value_of(t->schema, text, OYL_SCALAR_PLAIN, OYL_STR_NULL, v);
    return OYL_OK;
}

oyl_mark oyl_node_mark(const oyl_tree *t, uint32_t i) {
    if (!t || i >= t->len) return (oyl_mark){ 0, 0, 0 };
    size_t off = t->base + t->nodes[i].start;
    oyl_mark m = t->marks[i / MARK_EVERY];
    if (m.offset > off) m = t->doc_mark;   /* positions aren't always in node order */
    if (m.offset > off) return (oyl_mark){ off, 0, 0 };
    /* count line breaks as YAML does: \n, \r\n, or \r alone */
    for (size_t j = m.offset; j < off; j++) {
        char c = t->input[j];
        if (c == '\n' || (c == '\r' && (j + 1 >= t->input_len || t->input[j + 1] != '\n'))) {
            m.line++;
            m.col = 1;
        } else if (c != '\r') {
            m.col++;
        }
    }
    m.offset = off;
    return m;
}

/* ── Finding nodes ───────────────────────────────────────── */

uint32_t oyl_node_at(const oyl_tree *t, uint32_t seq, uint32_t k) {
    seq = oyl_node_target(t, seq);
    if (seq == NONE || t->nodes[seq].kind != OYL_NODE_SEQUENCE || k >= t->nodes[seq].len) return NONE;
    uint32_t c = seq + 1;
    while (k--) c += t->nodes[c].span;
    return c;
}

/* Is key node k a merge key? << untagged and plain, or tagged !!merge */
static bool is_merge_key(const oyl_tree *t, uint32_t k) {
    const oyl_node *n = &t->nodes[k];
    if (n->kind != OYL_NODE_SCALAR || n->len != 2 || memcmp(oyl_tree_text(t, n).data, "<<", 2) != 0)
        return false;
    oyl_str tag = written(t, n);
    return tag.len ? same(tag, OYL_TAG_MERGE) : n->style == OYL_SCALAR_PLAIN;
}

/* What value v of a << merges: v, or under YAML's merge the sequence an
 * alias refers to (go-yaml v2 takes only a mapping through an alias) */
static uint32_t merge_value(const oyl_tree *t, uint32_t v) {
    if (t->merge == OYL_MERGE_YAML && t->nodes[v].kind == OYL_NODE_ALIAS) {
        uint32_t s = oyl_node_target(t, v);
        if (s != NONE && t->nodes[s].kind == OYL_NODE_SEQUENCE) return s;
    }
    return v;
}

/* Can value v of a << in mapping m be merged? A mapping, an alias to one,
 * or a sequence of those, none of them m itself or a mapping containing
 * it. Aliases refer back, so a merge's source always ends before the
 * mapping merging it does: merges can't go round in a cycle. */
static bool mergeable(const oyl_tree *t, uint32_t m, uint32_t v) {
    const oyl_node *n = t->nodes;
    v = merge_value(t, v);
    bool seq = n[v].kind == OYL_NODE_SEQUENCE;
    uint32_t count = seq ? n[v].len : 1, item = seq ? v + 1 : v;
    for (uint32_t c = 0; c < count; c++, item += n[item].span) {
        uint32_t s = oyl_node_target(t, item);
        if (n[s].kind != OYL_NODE_MAPPING) return false;
        if (s <= m && m < s + n[s].span) return false;
    }
    return true;
}

/* A merge's items: the value, or a sequence's items */
static uint32_t merge_items(const oyl_tree *t, uint32_t v, uint32_t *first) {
    v = merge_value(t, v);
    bool seq = t->nodes[v].kind == OYL_NODE_SEQUENCE;
    *first = seq ? v + 1 : v;
    return seq ? t->nodes[v].len : 1;
}

/* Key k as keys are compared (k a scalar, not an alias) */
static void key_of(const oyl_tree *t, uint32_t k, oyl_key *key) {
    const oyl_node *n = &t->nodes[k];
    if (!n->props && (n->style != OYL_SCALAR_PLAIN || n->type == 0)) {   /* a string, as most keys are */
        *key = (oyl_key){ n->style != OYL_SCALAR_PLAIN ? t->schema->default_quoted_tag
                                                      : t->schema->default_plain_tag,
                          { .kind = OYL_VALUE_STR, .as = { .s = oyl_tree_text(t, n) } } };
        return;
    }
    oyl_value v;
    oyl_status st = oyl_node_value(t, k, &v);
    oyl_key_make(key, oyl_key_tag(t->schema, oyl_node_tag(t, k)), oyl_node_text(t, k), st, &v);
}

/* A lookup: by key, or by text */
typedef struct {
    const oyl_tree *t;
    const oyl_key  *key;    /* the key equal to this, or */
    oyl_str         text;   /* any scalar key with this text */
    struct { uint32_t map, key; } memo[64];   /* sources already searched */
    int             nmemo;
    size_t          budget;
} getter;

static bool matches(const getter *g, uint32_t k) {
    const oyl_tree *t = g->t;
    k = oyl_node_target(t, k);
    const oyl_node *n = &t->nodes[k];
    if (n->kind != OYL_NODE_SCALAR) return false;
    if (!g->key)
        return n->len == g->text.len &&
               (g->text.len == 0 || memcmp(oyl_tree_text(t, n).data, g->text.data, g->text.len) == 0);
    oyl_key key;
    key_of(t, k, &key);
    return oyl_key_cmp(&key, g->key) == 0;
}

static uint32_t get_in(getter *g, uint32_t m, int depth) {
    const oyl_tree *t = g->t;
    const oyl_node *n = t->nodes;
    for (int j = 0; j < g->nmemo; j++)
        if (g->memo[j].map == m) return g->memo[j].key;
    if (depth > MAX_MERGE_DEPTH) return NONE;
    bool merging = t->merge != OYL_MERGE_NONE, ordered = t->merge == OYL_MERGE_ORDERED;
    bool merges = false;
    uint32_t found = NONE, k = m + 1;
    for (uint32_t c = 0; c < n[m].len; c++) {
        uint32_t v = k + n[k].span, next = v + n[v].span;
        if (g->budget == 0) return NONE;
        g->budget--;
        if (merging && is_merge_key(t, k) && mergeable(t, m, v)) {
            merges = true;
            if (ordered) {   /* a merge sets what it has, the first item winning */
                uint32_t item, count = merge_items(t, v, &item);
                for (uint32_t j = 0; j < count; j++, item += n[item].span) {
                    uint32_t r = get_in(g, oyl_node_target(t, item), depth + 1);
                    if (r != NONE) { found = r; break; }
                }
            }
        } else if (matches(g, k)) {
            found = k;
        }
        k = next;
    }
    if (found == NONE && merges && !ordered) {
        /* YAML's merge: keys written win, then the merges, a later <<
         * over an earlier one (as PyYAML) and a sequence's earlier items
         * over its later ones */
        k = m + 1;
        for (uint32_t c = 0; c < n[m].len; c++) {
            uint32_t v = k + n[k].span, next = v + n[v].span;
            if (is_merge_key(t, k) && mergeable(t, m, v)) {
                uint32_t item, count = merge_items(t, v, &item), r = NONE;
                for (uint32_t j = 0; j < count && r == NONE; j++, item += n[item].span)
                    r = get_in(g, oyl_node_target(t, item), depth + 1);
                if (r != NONE) found = r;
            }
            k = next;
        }
    }
    if (depth > 0 && g->nmemo < (int)(sizeof g->memo / sizeof g->memo[0])) {
        g->memo[g->nmemo].map = m;
        g->memo[g->nmemo++].key = found;
    }
    return found;
}

/* The key in mapping `map` that counts for `key`, or NONE */
static uint32_t get_key(const oyl_tree *t, uint32_t map, const oyl_key *key, oyl_str text) {
    getter g = { .t = t, .key = key, .text = text, .budget = (size_t)t->len * 16 + 100000 };
    return get_in(&g, map, 0);
}

static uint32_t value_of(const oyl_tree *t, uint32_t k) {
    return k == NONE ? NONE : k + t->nodes[k].span;
}

uint32_t oyl_node_get(const oyl_tree *t, uint32_t map, oyl_str key) {
    map = oyl_node_target(t, map);
    if (map == NONE || t->nodes[map].kind != OYL_NODE_MAPPING) return NONE;
    /* the string, else the key a scalar written as `key` is (80 for "80") */
    oyl_key str = { t->schema->default_quoted_tag, { .kind = OYL_VALUE_STR, .as = { .s = key } } };
    uint32_t k = get_key(t, map, &str, key);
    if (k != NONE) return value_of(t, k);
    if ((k = get_key(t, map, NULL, key)) == NONE) return NONE;
    k = oyl_node_target(t, k);
    oyl_key written;
    key_of(t, k, &written);
    return value_of(t, get_key(t, map, &written, key));
}

uint32_t oyl_node_get_value(const oyl_tree *t, uint32_t map, const oyl_value *key) {
    map = oyl_node_target(t, map);
    if (!key || map == NONE || t->nodes[map].kind != OYL_NODE_MAPPING) return NONE;
    oyl_str tag;
    switch (key->kind) {
    case OYL_VALUE_NULL:  tag = OYL_TAG_NULL; break;
    case OYL_VALUE_BOOL:  tag = OYL_TAG_BOOL; break;
    case OYL_VALUE_INT:
    case OYL_VALUE_UINT:  tag = OYL_TAG_INT; break;
    case OYL_VALUE_FLOAT: tag = OYL_TAG_FLOAT; break;
    default:              tag = t->schema->default_quoted_tag; break;
    }
    oyl_key want;
    oyl_key_make(&want, tag, OYL_STR_NULL, OYL_OK, key);
    return value_of(t, get_key(t, map, &want, OYL_STR_NULL));
}

/* ── A mapping's entries ─────────────────────────────────── */

/* An entry while a mapping's entries are settled: of those with the same
 * key, the highest priority wins, at the first position */
typedef struct {
    uint32_t key, value;
    uint64_t prio;
    uint32_t pos;
    uint32_t form;      /* a scalar key's in the collector's forms; else NONE, by node */
    bool     written;   /* written in the mapping asked about */
} ent;

typedef struct {
    const oyl_tree *t;
    unsigned        flags;
    ent            *e, *tmp, *e_buf;
    uint32_t        len, cap, tmp_cap;
    oyl_key        *forms, *forms_buf;   /* the scalar keys as compared, apart: entries move */
    uint32_t        nforms, forms_cap;
    size_t          budget;
    oyl_status      status;
    uint32_t        bad;   /* the repeated key, under OYL_ENTRIES_STRICT */
} collector;

/* Room for one more in an array that starts in a buffer on the stack
 * (most mappings are small), and moves to the heap when it's full */
static bool room(void **data, const void *buf, uint32_t *cap, uint32_t len, size_t size) {
    if (len < *cap) return true;
    if (*data != buf) return grow_slow(data, cap, len, 1, size);
    void *p = malloc((size_t)*cap * 2 * size);
    if (!p) return false;
    memcpy(p, buf, (size_t)len * size);
    *data = p;
    *cap *= 2;
    return true;
}

static bool push_ent(collector *c, ent e) {
    if (c->budget == 0) { c->status = OYL_ERR_LIMIT; return false; }
    c->budget--;
    if (!room((void **)&c->e, c->e_buf, &c->cap, c->len, sizeof *c->e)) { c->status = OYL_ERR_MEMORY; return false; }
    uint32_t k = oyl_node_target(c->t, e.key);
    e.form = NONE;
    if (c->t->nodes[k].kind == OYL_NODE_SCALAR) {
        if (!room((void **)&c->forms, c->forms_buf, &c->forms_cap, c->nforms, sizeof *c->forms)) {
            c->status = OYL_ERR_MEMORY;
            return false;
        }
        key_of(c->t, k, &c->forms[c->nforms]);
        e.form = c->nforms++;
    }
    c->e[c->len++] = e;
    return true;
}

/* Order by key (scalars by tag and value, collections by node), then
 * priority */
static int by_key(const collector *c, const ent *a, const ent *b) {
    bool sa = a->form != NONE, sb = b->form != NONE;
    if (sa != sb) return sa ? -1 : 1;
    if (sa) {
        int r = oyl_key_cmp(&c->forms[a->form], &c->forms[b->form]);
        if (r) return r;
    } else {
        uint32_t ka = oyl_node_target(c->t, a->key), kb = oyl_node_target(c->t, b->key);
        if (ka != kb) return ka < kb ? -1 : 1;
    }
    return a->prio < b->prio ? -1 : a->prio > b->prio;
}

static int by_pos(const collector *c, const ent *a, const ent *b) {
    (void)c;
    return a->pos < b->pos ? -1 : a->pos > b->pos;
}

typedef int (*ent_cmp)(const collector *, const ent *, const ent *);

static void sort_ents(const collector *c, ent *e, ent *tmp, uint32_t n, ent_cmp cmp) {
    if (n < 2) return;
    if (n <= 16) {   /* insertion sort */
        for (uint32_t i = 1; i < n; i++) {
            ent x = e[i];
            uint32_t j = i;
            while (j > 0 && cmp(c, &e[j - 1], &x) > 0) { e[j] = e[j - 1]; j--; }
            e[j] = x;
        }
        return;
    }
    uint32_t h = n / 2;
    sort_ents(c, e, tmp, h, cmp);
    sort_ents(c, e + h, tmp, n - h, cmp);
    uint32_t i = 0, j = h, k = 0;
    while (i < h && j < n) tmp[k++] = cmp(c, &e[j], &e[i]) < 0 ? e[j++] : e[i++];
    while (i < h) tmp[k++] = e[i++];
    while (j < n) tmp[k++] = e[j++];
    memcpy(e, tmp, n * sizeof *e);
}

static bool same_key(const collector *c, const ent *a, const ent *b) {
    ent x = *a, y = *b;
    x.prio = y.prio = 0;
    return by_key(c, &x, &y) == 0;
}

/* Settle the entries from `base`: one per key, the highest priority at the
 * first position, in order of position */
static bool settle(collector *c, uint32_t base, bool top) {
    uint32_t n = c->len - base;
    if (n == 0) return true;
    if (n > 16 && n > c->tmp_cap) {
        ent *tmp = realloc(c->tmp, n * sizeof *tmp);
        if (!tmp) { c->status = OYL_ERR_MEMORY; return false; }
        c->tmp = tmp;
        c->tmp_cap = n;
    }
    ent *e = c->e + base;
    sort_ents(c, e, c->tmp, n, by_key);
    uint32_t out = 0, repeat = NONE;   /* the first key written again */
    for (uint32_t i = 0; i < n;) {
        uint32_t j = i + 1, pos = e[i].pos, written = e[i].written;
        while (j < n && same_key(c, &e[j], &e[i])) {
            if (e[j].pos < pos) pos = e[j].pos;
            written += e[j++].written;
        }
        if (top && written > 1 && (c->flags & OYL_ENTRIES_STRICT)) {
            uint32_t first = NONE, second = NONE;
            for (uint32_t w = i; w < j; w++) {
                if (!e[w].written) continue;
                if (e[w].key < first) { second = first; first = e[w].key; }
                else if (e[w].key < second) second = e[w].key;
            }
            if (second < repeat) repeat = second;
        }
        e[out] = e[j - 1];
        e[out++].pos = pos;
        i = j;
    }
    if (repeat != NONE) {
        c->status = OYL_ERR_PARSE;
        c->bad = repeat;
        return false;
    }
    sort_ents(c, e, c->tmp, out, by_pos);
    c->len = base + out;
    return true;
}

/* Append mapping m's entries, settled, to c->e */
static bool collect(collector *c, uint32_t m, int depth, bool top) {
    const oyl_tree *t = c->t;
    const oyl_node *n = t->nodes;
    if (depth > MAX_MERGE_DEPTH) { c->status = OYL_ERR_LIMIT; return false; }
    bool merging = t->merge != OYL_MERGE_NONE, ordered = t->merge == OYL_MERGE_ORDERED;
    uint32_t base = c->len, pos = 0;
    uint64_t counter = 0, merged = 0;
    const uint64_t written_prio = (uint64_t)1 << 63, merged_prio = (uint64_t)1 << 62;
    uint32_t k = m + 1;
    for (uint32_t i = 0; i < n[m].len; i++) {
        uint32_t v = k + n[k].span, next = v + n[v].span;
        if (merging && is_merge_key(t, k) && mergeable(t, m, v)) {
            uint32_t item, count = merge_items(t, v, &item);
            for (uint32_t j = 0; j < count; j++, item += n[item].span) {
                uint32_t from = c->len;
                if (!collect(c, oyl_node_target(t, item), depth + 1, false)) return false;
                /* go-yaml v2: a merge overrides what came before, its first
                 * item winning; YAML: keys written win, then the merges, a
                 * later << over an earlier one (as PyYAML), its first item
                 * winning */
                uint64_t prio = ordered ? counter + (count - j) : merged_prio + merged + (count - j);
                for (uint32_t x = from; x < c->len; x++) {
                    c->e[x].prio = prio;
                    c->e[x].pos = pos++;
                    c->e[x].written = false;
                }
            }
            if (ordered) counter += count;
            else merged += count;
        } else {
            uint64_t prio = ordered || !merging ? ++counter : written_prio + ++counter;
            if (!push_ent(c, (ent){ .key = k, .value = v, .prio = prio, .pos = pos++, .written = top })) return false;
        }
        k = next;
    }
    return settle(c, base, top);
}

oyl_status oyl_node_entries(const oyl_tree *t, uint32_t map, unsigned flags, oyl_arena *a,
                            const oyl_pair **pairs, size_t *n) {
    if (!pairs || !n) return OYL_ERR_INPUT;
    *pairs = NULL;
    *n = 0;
    if (!t || !a) return OYL_ERR_INPUT;
    map = oyl_node_target(t, map);
    if (map == NONE || t->nodes[map].kind != OYL_NODE_MAPPING) return OYL_ERR_INPUT;
    ent e_buf[32];
    oyl_key forms_buf[32];
    collector c = { .t = t, .flags = flags, .e = e_buf, .e_buf = e_buf, .cap = 32,
                    .forms = forms_buf, .forms_buf = forms_buf, .forms_cap = 32,
                    .budget = (size_t)t->len * 16 + 100000 };
    oyl_status st = OYL_OK;
    if (!collect(&c, map, 0, true)) {
        st = c.status;
        if (st == OYL_ERR_PARSE) *n = c.bad;
    } else {
        oyl_pair *out = oyl_arena_alloc(a, (c.len ? c.len : 1) * sizeof *out, _Alignof(oyl_pair));
        if (!out) {
            st = OYL_ERR_MEMORY;
        } else {
            for (uint32_t i = 0; i < c.len; i++) out[i] = (oyl_pair){ c.e[i].key, c.e[i].value };
            *pairs = out;
            *n = c.len;
        }
    }
    if (c.e != e_buf) free(c.e);
    if (c.forms != forms_buf) free(c.forms);
    free(c.tmp);
    return st;
}

/* ── Writing ─────────────────────────────────────────────── */

/* Emit node i, as the event that read it would be */
static oyl_status emit_node(oyl_emitter *e, const oyl_tree *t, uint32_t i) {
    const oyl_node *n = &t->nodes[i];
    if (n->kind == OYL_NODE_ALIAS) return oyl_emit_alias(e, oyl_tree_text(t, n));
    oyl_str tag = OYL_STR_NULL, anchor = OYL_STR_NULL;
    if (n->props) {
        const oyl_node_props *pr = &t->props[n->props];
        tag = pooled(t, pr->tag, pr->tag_len);
        anchor = pooled(t, pr->anchor, pr->anchor_len);
    }
    bool flow = (n->flags & OYL_NODE_FLOW) != 0;
    switch (n->kind) {
    case OYL_NODE_SCALAR:
        return oyl_emit_scalar(e, oyl_tree_text(t, n), (oyl_scalar_style)n->style, anchor, tag);
    case OYL_NODE_SEQUENCE: return oyl_emit_sequence_start(e, anchor, tag, flow);
    default:                return oyl_emit_mapping_start(e, anchor, tag, flow);
    }
}

/* End the collections from node j up through its ancestors, stopping at
 * `stop` (still open; NONE ends them all) */
static oyl_status end_up_to(oyl_emitter *e, const oyl_tree *t, uint32_t j, uint32_t stop) {
    const oyl_node *n = t->nodes;
    oyl_status st = OYL_OK;
    while (j != stop && st == OYL_OK) {
        if (n[j].kind == OYL_NODE_SEQUENCE) st = oyl_emit_sequence_end(e);
        else if (n[j].kind == OYL_NODE_MAPPING) st = oyl_emit_mapping_end(e);
        if (j == 0) break;
        j -= n[j].parent;
    }
    return st;
}

oyl_status oyl_emit_tree(oyl_emitter *e, const oyl_tree *t) {
    if (!e || !t) return OYL_ERR_INPUT;
    oyl_status st = oyl_emit_document_start(e, t->implicit_start);
    for (uint32_t i = 0; i < t->len && st == OYL_OK; i++) {
        /* node i's parent is node i-1 or one of its ancestors: the
         * collections between them have ended */
        if (i) st = end_up_to(e, t, i - 1, i - t->nodes[i].parent);
        if (st == OYL_OK) st = emit_node(e, t, i);
    }
    if (st == OYL_OK) st = end_up_to(e, t, t->len - 1, NONE);
    if (st == OYL_OK) st = oyl_emit_document_end(e, t->implicit_end);
    return st;
}
