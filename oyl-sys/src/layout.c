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
};
const size_t oyl_sys_layout_len = sizeof oyl_sys_layout / sizeof oyl_sys_layout[0];
