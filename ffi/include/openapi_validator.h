/*
 * openapi-validator C ABI.
 *
 * Validates HTTP requests and responses against an OpenAPI 3.0 or 3.1
 * document. Validation is synchronous and runs on the calling thread; the
 * library starts no threads.
 *
 * MEMORY OWNERSHIP
 *   - Inputs are borrowed for the duration of the call only. Every string is
 *     passed as a pointer and a byte length and need not be NUL-terminated.
 *     A null pointer with length 0 is an empty string.
 *   - oav_spec_load hands back an oav_spec that the caller owns and frees
 *     with oav_spec_free. One oav_spec may be shared by any number of
 *     threads at once; it must not be freed while a call is using it.
 *   - oav_validate_request / oav_validate_response hand back an oav_report
 *     that the caller owns and frees with oav_report_free. Problem strings
 *     read with oav_report_problem borrow from the report and stay valid
 *     until it is freed; they are NOT NUL-terminated (use the lengths).
 *   - Strings returned as `char *` (load errors, oav_report_to_string) are
 *     NUL-terminated, owned by the caller, and freed with oav_string_free.
 *   - oav_version and problem kinds are static; never free them.
 *   - All free functions accept null.
 *
 * ERRORS
 *   Every function that can fail returns an int: OAV_OK (0) or an OAV_ERR_*
 *   code. Panics inside the library are caught and reported as
 *   OAV_ERR_PANIC; no unwind ever crosses this boundary. A validation that
 *   finds problems still returns OAV_OK: the problems are in the report.
 *
 * SECURITY HOOK
 *   When oav_options.security_hook is set, it is called for every
 *   credential that is present, possibly from several threads at once, with
 *   security_hook_data passed back unchanged. Return 0 to accept the
 *   credential and anything else to refuse it. The credential's strings are
 *   valid only during the call. The hook must not call back into this
 *   library with the same oav_spec.
 *
 * STACK
 *   The worst documents the loader accepts validate within 256 KiB of stack
 *   in release builds (see the README).
 */
#ifndef OPENAPI_VALIDATOR_H
#define OPENAPI_VALIDATOR_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OAV_OK 0
#define OAV_ERR_NULL 1   /* a required pointer was null */
#define OAV_ERR_LOAD 2   /* the document was refused; see the error string */
#define OAV_ERR_UTF8 3   /* method, path or query is not UTF-8 */
#define OAV_ERR_PANIC 4  /* an internal fault was caught */
#define OAV_ERR_RANGE 5  /* an index or enum value is out of range */

typedef struct oav_spec oav_spec;
typedef struct oav_report oav_report;

typedef struct {
    const char *name;
    size_t name_len;
    const char *value;
    size_t value_len;
} oav_header;

typedef struct {
    const char *method; /* as sent, e.g. "GET" (case-sensitive) */
    size_t method_len;
    const char *path;   /* as sent, still percent-encoded, no query */
    size_t path_len;
    const char *query;  /* raw query string without '?' */
    size_t query_len;
    const oav_header *headers; /* in arrival order, repeats kept */
    size_t headers_len;
    const uint8_t *body;
    size_t body_len;
} oav_request;

typedef struct {
    uint16_t status;
    const oav_header *headers;
    size_t headers_len;
    const uint8_t *body;
    size_t body_len;
} oav_response;

typedef struct {
    const char *scheme_name; /* name under components.securitySchemes */
    size_t scheme_name_len;
    const char *scheme_type; /* apiKey, http, oauth2, openIdConnect */
    size_t scheme_type_len;
    int location;            /* 0 query, 1 header, 2 cookie */
    const char *value;       /* the key, or the credential after the scheme */
    size_t value_len;
} oav_credential;

typedef int (*oav_security_hook)(void *user_data, const oav_credential *credential);

typedef struct {
    bool validate_body;                /* default true */
    bool allow_unknown_query;          /* default true */
    bool allow_unknown_headers;        /* default true */
    bool allow_unknown_cookies;        /* default true */
    bool check_security;               /* default true */
    bool query_plus_as_space;          /* default true */
    int formats;                       /* 0 by version (default), 1 always, 2 never */
    int discriminator;                 /* 0 annotation (default), 1 enforce */
    int integers;                      /* 0 mathematical (default), 1 lexical in 3.0 */
    int read_write_required;           /* 0 by version (default), 1 relax in 3.1 too */
    int patterns;                      /* 0 bounded backtracking (default), 1 refuse at load */
    int server_paths;                  /* 0 either (default), 1 required, 2 ignored */
    size_t max_problems;               /* default 20 */
    size_t max_report_bytes;           /* default 8192 */
    size_t max_body_bytes;             /* default 16 MiB */
    size_t max_body_depth;             /* default 128 */
    oav_security_hook security_hook;   /* optional */
    void *security_hook_data;
} oav_options;

typedef struct {
    const char *kind;     /* static, NUL-terminated, e.g. "invalid_parameter" */
    const char *location; /* e.g. "query.limit", "body/items/3"; not NUL-terminated */
    size_t location_len;
    const char *message;  /* one sentence; not NUL-terminated */
    size_t message_len;
} oav_problem;

/* Fills *opts with the defaults. Start from this, then change fields. */
int oav_options_default(oav_options *opts);

/* Loads a document (JSON or YAML). opts may be null for the defaults.
 * On OAV_OK, *out is the validator. On OAV_ERR_LOAD, *error (if error is
 * not null) is a message to free with oav_string_free. */
int oav_spec_load(const uint8_t *doc, size_t doc_len, const oav_options *opts,
                  oav_spec **out, char **error);
typedef struct {
    const char *uri;    /* the URI references use for this document */
    size_t uri_len;
    const uint8_t *data;
    size_t data_len;
} oav_document;

/* Loads a description spread over several documents. `entry` is the OpenAPI
 * document at `entry_uri`; `others` are the documents it refers to. Nothing
 * is fetched: a reference to a document not supplied is a load error.
 * Ownership as for oav_spec_load. */
int oav_spec_load_documents(const char *entry_uri, size_t entry_uri_len,
                            const uint8_t *entry, size_t entry_len,
                            const oav_document *others, size_t others_len,
                            const oav_options *opts, oav_spec **out, char **error);
void oav_spec_free(oav_spec *spec);

int oav_validate_request(const oav_spec *spec, const oav_request *request,
                         oav_report **out);
int oav_validate_response(const oav_spec *spec, const oav_request *request,
                          const oav_response *response, oav_report **out);

/* 1 when the report has no problems, else 0. */
int oav_report_valid(const oav_report *report);
/* Problems kept, and problems found past the report bounds ("and N more"). */
size_t oav_report_len(const oav_report *report);
size_t oav_report_omitted(const oav_report *report);
int oav_report_problem(const oav_report *report, size_t index, oav_problem *out);
char *oav_report_to_string(const oav_report *report);
void oav_report_free(oav_report *report);

void oav_string_free(char *s);
const char *oav_version(void);

#ifdef __cplusplus
}
#endif

#endif /* OPENAPI_VALIDATOR_H */
