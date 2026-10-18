/* The sizes and field offsets of the C structs that src/lib.rs mirrors,
 * so its layout test can check them. */
#include <stddef.h>
#include "oyl/oyl.h"

const size_t oyl_sys_layout[] = {
    sizeof(oyl_str), offsetof(oyl_str, data), offsetof(oyl_str, len),
    sizeof(oyl_mark), offsetof(oyl_mark, offset), offsetof(oyl_mark, line),
    offsetof(oyl_mark, col),
    sizeof(oyl_token), offsetof(oyl_token, type), offsetof(oyl_token, value),
    offsetof(oyl_token, scalar_style), offsetof(oyl_token, start),
    offsetof(oyl_token, end),
    sizeof(oyl_event), offsetof(oyl_event, type), offsetof(oyl_event, value),
    offsetof(oyl_event, anchor), offsetof(oyl_event, tag),
    offsetof(oyl_event, scalar_style), offsetof(oyl_event, implicit),
    offsetof(oyl_event, flow), offsetof(oyl_event, start),
    offsetof(oyl_event, end),
    sizeof(oyl_status), sizeof(oyl_event_type),
    sizeof(oyl_value), offsetof(oyl_value, kind), offsetof(oyl_value, as),
    sizeof(oyl_node), offsetof(oyl_node, text), offsetof(oyl_node, len),
    offsetof(oyl_node, span), offsetof(oyl_node, parent), offsetof(oyl_node, props),
    offsetof(oyl_node, start), offsetof(oyl_node, end), offsetof(oyl_node, kind),
    offsetof(oyl_node, style), offsetof(oyl_node, type), offsetof(oyl_node, flags),
    sizeof(oyl_node_props), offsetof(oyl_node_props, tag),
    offsetof(oyl_node_props, tag_len), offsetof(oyl_node_props, anchor),
    offsetof(oyl_node_props, anchor_len),
    sizeof(oyl_pair), offsetof(oyl_pair, key), offsetof(oyl_pair, value),
};
const size_t oyl_sys_layout_len = sizeof oyl_sys_layout / sizeof oyl_sys_layout[0];
