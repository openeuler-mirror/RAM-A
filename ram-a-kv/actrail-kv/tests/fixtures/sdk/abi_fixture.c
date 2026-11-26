/* A C ABI implementation used to verify unchanged-host upgrades and version rejection. */
#include "actrail.h"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#ifndef ACTRAIL_TEST_ABI
#define ACTRAIL_TEST_ABI 1
#endif
#ifndef ACTRAIL_TEST_REVISION
#define ACTRAIL_TEST_REVISION 1
#endif
static int32_t respond(actrail_kv_output *output, const char *data) {
    char text[256];
    int size = snprintf(text, sizeof text, "{\"schema_version\":1,\"ok\":true,\"data\":%s}", data);
    output->len = (size_t)size;
    output->ptr = malloc(output->len);
    if (!output->ptr) abort();
    memcpy(output->ptr, text, output->len);
    return 0;
}
uint32_t actrail_kv_analyzer_abi_major(void) { return ACTRAIL_TEST_ABI; }
uint32_t actrail_kv_analyzer_abi_minor(void) { return 0; }
int32_t actrail_kv_analyzer_create(actrail_kv_input input, actrail_kv_handle *handle, actrail_kv_output *output) {
    (void)input;
    *handle = malloc(1);
    if (!*handle) abort();
    return respond(output, "{}");
}
#ifndef ACTRAIL_TEST_MISSING
int32_t actrail_kv_analyzer_analyze(actrail_kv_handle handle, actrail_kv_input input, actrail_kv_output *output) {
    (void)handle; (void)input;
    char data[64];
    snprintf(data, sizeof data, "{\"revision\":%d}", ACTRAIL_TEST_REVISION);
    return respond(output, data);
}
#endif
int32_t actrail_kv_analyzer_destroy(actrail_kv_handle handle, actrail_kv_output *output) {
    free(handle);
    return respond(output, "{}");
}
void actrail_kv_analyzer_buffer_free(actrail_kv_output output) { free(output.ptr); }
int32_t actrail_kv_analyzer_export(actrail_kv_input input, actrail_kv_input destination, actrail_kv_output *output) {
    (void)input; (void)destination;
    return respond(output, "{}");
}
