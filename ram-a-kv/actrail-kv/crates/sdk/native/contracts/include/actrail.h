/* actrail-kv C ABI v1: borrowed JSON inputs, library-owned outputs and opaque instances. */
#ifndef ACTRAIL_KV_H
#define ACTRAIL_KV_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct { const uint8_t *ptr; size_t len; } actrail_kv_input;
typedef struct { uint8_t *ptr; size_t len; } actrail_kv_output;
typedef void *actrail_kv_handle;

enum actrail_kv_status {
    ACTRAIL_KV_OK = 0,
    ACTRAIL_KV_INPUT_ERROR = 1,
    ACTRAIL_KV_CONFIG_ERROR = 2,
    ACTRAIL_KV_CAPTURE_INPUT_ERROR = 3,
    ACTRAIL_KV_STORAGE_ERROR = 4,
    ACTRAIL_KV_OPERATION_ERROR = 5,
    ACTRAIL_KV_INTERNAL_ERROR = 6
};

/* Inputs remain valid for the full call. A handle has one exclusive caller.
 * Each returned output must be freed once by the allocating library.
 * destroy consumes the handle even on failure. Outputs outlive handles.
 * Zero-length null buffers are allowed, but empty JSON is invalid.
 * Version functions must be checked before using the remaining symbols. */
#define ACTRAIL_KV_LIFECYCLE(name) \
uint32_t actrail_kv_##name##_abi_major(void); \
uint32_t actrail_kv_##name##_abi_minor(void); \
int32_t actrail_kv_##name##_create(actrail_kv_input config, actrail_kv_handle *handle, actrail_kv_output *response); \
int32_t actrail_kv_##name##_destroy(actrail_kv_handle handle, actrail_kv_output *response); \
void actrail_kv_##name##_buffer_free(actrail_kv_output response)

ACTRAIL_KV_LIFECYCLE(receiver);
ACTRAIL_KV_LIFECYCLE(analyzer);
ACTRAIL_KV_LIFECYCLE(reporter);
#undef ACTRAIL_KV_LIFECYCLE

int32_t actrail_kv_receiver_submit(actrail_kv_handle, actrail_kv_input, actrail_kv_output *);
int32_t actrail_kv_analyzer_analyze(actrail_kv_handle, actrail_kv_input, actrail_kv_output *);
int32_t actrail_kv_reporter_render(actrail_kv_handle, actrail_kv_input analysis_response, actrail_kv_input options, actrail_kv_output *);
int32_t actrail_kv_analyzer_export(actrail_kv_input analysis_response, actrail_kv_input destination, actrail_kv_output *);
int32_t actrail_kv_reporter_export(actrail_kv_input report_response, actrail_kv_input destination, actrail_kv_output *);
#ifdef __cplusplus
}
#endif
#endif
