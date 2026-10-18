/*
 * oyl_dialect.c — dialects: a schema, syntax leniencies, and a merge order
 *
 * A document chooses its dialect with directives (oyl_parser.c,
 * select_dialect); these are the built-in ones and the constructor.
 */

#include "oyl_internal.h"

static const oyl_dialect yaml12 = { .schema = NULL, .syntax = 0, .merge = OYL_MERGE_YAML };
static const oyl_dialect yaml11 = { .schema = NULL, .syntax = 0, .merge = OYL_MERGE_YAML };
static const oyl_dialect goyaml2 = { .schema = NULL, .syntax = OYL_SYNTAX_LIBYAML,
                                     .merge = OYL_MERGE_ORDERED };

/* The presets' schemas are functions' results, not constants, so the
 * accessor fills them in */
const oyl_dialect *oyl_dialect_yaml12(void) { return &yaml12; }
const oyl_dialect *oyl_dialect_yaml11(void) { return &yaml11; }
const oyl_dialect *oyl_dialect_goyaml2(void) { return &goyaml2; }

const oyl_schema *oyl_dialect_schema(const oyl_dialect *d) {
    if (!d) return NULL;
    if (d->schema) return d->schema;
    if (d == &yaml11) return oyl_schema_yaml11();
    if (d == &goyaml2) return oyl_schema_goyaml2();
    return oyl_schema_core();
}

unsigned oyl_dialect_syntax(const oyl_dialect *d) { return d ? d->syntax : 0; }

oyl_merge_order oyl_dialect_merge(const oyl_dialect *d) { return d ? d->merge : OYL_MERGE_YAML; }

const oyl_dialect *oyl_dialect_new(oyl_arena *a, const oyl_schema *schema, unsigned syntax,
                                   oyl_merge_order merge) {
    oyl_dialect *d = oyl_arena_alloc(a, sizeof *d, _Alignof(oyl_dialect));
    if (!d) return NULL;
    *d = (oyl_dialect){ .schema = schema ? schema : oyl_schema_core(), .syntax = syntax,
                        .merge = merge };
    return d;
}
