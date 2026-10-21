//! C ABI for `openapi-validator`. The contract, including memory
//! ownership, is in `include/openapi_validator.h`.
//!
//! Every entry point catches panics and turns them into `OAV_ERR_PANIC`, so
//! no unwind ever crosses into C (or Go). Validation runs synchronously on
//! the caller's thread.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use openapi_validator::{
    Credential, CredentialLocation, DiscriminatorMode, FormatAssertion, IntegerSemantics, Options,
    PatternEngine, ReadWriteRequired, Report, Request, Response, ServerPaths, Spec,
};

pub const OAV_OK: c_int = 0;
pub const OAV_ERR_NULL: c_int = 1;
pub const OAV_ERR_LOAD: c_int = 2;
pub const OAV_ERR_UTF8: c_int = 3;
pub const OAV_ERR_PANIC: c_int = 4;
pub const OAV_ERR_RANGE: c_int = 5;

/// Opaque validator.
pub struct OavSpec(Spec);

/// Opaque report.
pub struct OavReport(Report);

#[repr(C)]
pub struct OavHeader {
    pub name: *const c_char,
    pub name_len: usize,
    pub value: *const c_char,
    pub value_len: usize,
}

#[repr(C)]
pub struct OavRequest {
    pub method: *const c_char,
    pub method_len: usize,
    pub path: *const c_char,
    pub path_len: usize,
    pub query: *const c_char,
    pub query_len: usize,
    pub headers: *const OavHeader,
    pub headers_len: usize,
    pub body: *const u8,
    pub body_len: usize,
}

#[repr(C)]
pub struct OavResponse {
    pub status: u16,
    pub headers: *const OavHeader,
    pub headers_len: usize,
    pub body: *const u8,
    pub body_len: usize,
}

#[repr(C)]
pub struct OavCredential {
    pub scheme_name: *const c_char,
    pub scheme_name_len: usize,
    pub scheme_type: *const c_char,
    pub scheme_type_len: usize,
    /// 0 query, 1 header, 2 cookie.
    pub location: c_int,
    pub value: *const c_char,
    pub value_len: usize,
}

/// Returns 0 to accept the credential, anything else to refuse it.
pub type OavSecurityHook =
    Option<unsafe extern "C" fn(user_data: *mut c_void, credential: *const OavCredential) -> c_int>;

#[repr(C)]
pub struct OavOptions {
    pub validate_body: bool,
    pub allow_unknown_query: bool,
    pub allow_unknown_headers: bool,
    pub allow_unknown_cookies: bool,
    pub check_security: bool,
    pub query_plus_as_space: bool,
    /// 0 by version (default), 1 always, 2 never.
    pub formats: c_int,
    /// 0 annotation (default), 1 enforce.
    pub discriminator: c_int,
    /// 0 mathematical (default), 1 lexical in OpenAPI 3.0.
    pub integers: c_int,
    /// 0 by version (default), 1 relax `required` in 3.1 too.
    pub read_write_required: c_int,
    /// 0 bounded backtracking (default), 1 refuse such patterns at load.
    pub patterns: c_int,
    /// Refuse documents that break a MUST of the specification.
    pub strict_document: bool,
    /// 0 either, 1 required, 2 ignored.
    pub server_paths: c_int,
    pub max_problems: usize,
    pub max_report_bytes: usize,
    pub max_body_bytes: usize,
    pub max_body_depth: usize,
    pub security_hook: OavSecurityHook,
    pub security_hook_data: *mut c_void,
}

#[repr(C)]
pub struct OavProblem {
    /// Static, NUL-terminated.
    pub kind: *const c_char,
    pub location: *const c_char,
    pub location_len: usize,
    pub message: *const c_char,
    pub message_len: usize,
}

/// Raw pointer wrapper so the hook's user data can live in `Options`.
struct HookData(*mut c_void);
// SAFETY: the header requires the caller's hook and user data to be safe
// to call from any thread that calls the validator.
unsafe impl Send for HookData {}
unsafe impl Sync for HookData {}

fn guard(f: impl FnOnce() -> c_int) -> c_int {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(OAV_ERR_PANIC)
}

/// A borrowed byte slice from a pointer and length; null with length 0 is
/// the empty slice.
unsafe fn bytes<'a>(p: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `p` points at `len` readable bytes that
    // outlive the call.
    Some(unsafe { std::slice::from_raw_parts(p, len) })
}

unsafe fn text<'a>(p: *const c_char, len: usize) -> Result<&'a str, c_int> {
    let b = unsafe { bytes(p.cast(), len) }.ok_or(OAV_ERR_NULL)?;
    std::str::from_utf8(b).map_err(|_| OAV_ERR_UTF8)
}

/// Header bytes as text. Values that are not UTF-8 are read lossily, which
/// can only make them fail a schema, never pass one they should not.
unsafe fn headers(p: *const OavHeader, len: usize) -> Result<Vec<(String, String)>, c_int> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if p.is_null() {
        return Err(OAV_ERR_NULL);
    }
    // SAFETY: the caller guarantees `len` valid headers.
    let hs = unsafe { std::slice::from_raw_parts(p, len) };
    let mut out = Vec::with_capacity(len);
    for h in hs {
        let n = unsafe { bytes(h.name.cast(), h.name_len) }.ok_or(OAV_ERR_NULL)?;
        let v = unsafe { bytes(h.value.cast(), h.value_len) }.ok_or(OAV_ERR_NULL)?;
        out.push((
            String::from_utf8_lossy(n).into_owned(),
            String::from_utf8_lossy(v).into_owned(),
        ));
    }
    Ok(out)
}

/// Fills `opts` with the library defaults.
#[no_mangle]
pub unsafe extern "C" fn oav_options_default(opts: *mut OavOptions) -> c_int {
    guard(|| {
        if opts.is_null() {
            return OAV_ERR_NULL;
        }
        let d = Options::default();
        let v = OavOptions {
            validate_body: d.validate_body,
            allow_unknown_query: d.allow_unknown_query,
            allow_unknown_headers: d.allow_unknown_headers,
            allow_unknown_cookies: d.allow_unknown_cookies,
            check_security: d.check_security,
            query_plus_as_space: d.query_plus_as_space,
            formats: 0,
            discriminator: 0,
            integers: 0,
            read_write_required: 0,
            patterns: 0,
            strict_document: d.strict_document,
            server_paths: 0,
            max_problems: d.max_problems,
            max_report_bytes: d.max_report_bytes,
            max_body_bytes: d.max_body_bytes,
            max_body_depth: d.max_body_depth,
            security_hook: None,
            security_hook_data: std::ptr::null_mut(),
        };
        // SAFETY: `opts` is non-null and points at writable storage.
        unsafe { opts.write(v) };
        OAV_OK
    })
}

fn to_options(o: &OavOptions) -> Result<Options, c_int> {
    let mut out = Options {
        validate_body: o.validate_body,
        allow_unknown_query: o.allow_unknown_query,
        allow_unknown_headers: o.allow_unknown_headers,
        allow_unknown_cookies: o.allow_unknown_cookies,
        check_security: o.check_security,
        query_plus_as_space: o.query_plus_as_space,
        formats: match o.formats {
            0 => FormatAssertion::ByVersion,
            1 => FormatAssertion::Always,
            2 => FormatAssertion::Never,
            _ => return Err(OAV_ERR_RANGE),
        },
        discriminator: match o.discriminator {
            0 => DiscriminatorMode::Annotation,
            1 => DiscriminatorMode::Enforce,
            _ => return Err(OAV_ERR_RANGE),
        },
        integers: match o.integers {
            0 => IntegerSemantics::Mathematical,
            1 => IntegerSemantics::LexicalIn30,
            _ => return Err(OAV_ERR_RANGE),
        },
        read_write_required: match o.read_write_required {
            0 => ReadWriteRequired::ByVersion,
            1 => ReadWriteRequired::RelaxAlways,
            _ => return Err(OAV_ERR_RANGE),
        },
        strict_document: o.strict_document,
        patterns: match o.patterns {
            0 => PatternEngine::Bounded,
            1 => PatternEngine::LinearOnly,
            _ => return Err(OAV_ERR_RANGE),
        },
        server_paths: match o.server_paths {
            0 => ServerPaths::Either,
            1 => ServerPaths::Required,
            2 => ServerPaths::Ignored,
            _ => return Err(OAV_ERR_RANGE),
        },
        max_problems: o.max_problems,
        max_report_bytes: o.max_report_bytes,
        max_body_bytes: o.max_body_bytes,
        max_body_depth: o.max_body_depth,
        ..Options::default()
    };
    if let Some(hook) = o.security_hook {
        let data = Arc::new(HookData(o.security_hook_data));
        out.security_hook = Some(Arc::new(move |c: &Credential<'_>| {
            let cred = OavCredential {
                scheme_name: c.scheme_name.as_ptr().cast(),
                scheme_name_len: c.scheme_name.len(),
                scheme_type: c.scheme_type.as_ptr().cast(),
                scheme_type_len: c.scheme_type.len(),
                location: match c.location {
                    CredentialLocation::Query => 0,
                    CredentialLocation::Header => 1,
                    CredentialLocation::Cookie => 2,
                },
                value: c.value.as_ptr().cast(),
                value_len: c.value.len(),
            };
            // SAFETY: the hook contract is documented in the header; the
            // credential is valid for the duration of the call.
            let rc = unsafe { hook(data.0, &cred) };
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("refused by the security hook ({rc})"))
            }
        }));
    }
    Ok(out)
}

/// Loads a document. On success `*out` owns a validator to free with
/// `oav_spec_free`. On `OAV_ERR_LOAD`, `*error` (when not null) receives a
/// message to free with `oav_string_free`.
#[no_mangle]
pub unsafe extern "C" fn oav_spec_load(
    doc: *const u8,
    doc_len: usize,
    opts: *const OavOptions,
    out: *mut *mut OavSpec,
    error: *mut *mut c_char,
) -> c_int {
    guard(|| {
        if out.is_null() {
            return OAV_ERR_NULL;
        }
        // SAFETY: `out` is non-null; set it before any early return.
        unsafe { out.write(std::ptr::null_mut()) };
        if !error.is_null() {
            unsafe { error.write(std::ptr::null_mut()) };
        }
        let Some(doc) = (unsafe { bytes(doc, doc_len) }) else {
            return OAV_ERR_NULL;
        };
        let options = if opts.is_null() {
            Options::default()
        } else {
            // SAFETY: non-null and valid per the header.
            match to_options(unsafe { &*opts }) {
                Ok(o) => o,
                Err(e) => return e,
            }
        };
        match Spec::load(doc, options) {
            Ok(spec) => {
                unsafe { out.write(Box::into_raw(Box::new(OavSpec(spec)))) };
                OAV_OK
            }
            Err(e) => {
                if !error.is_null() {
                    let msg = CString::new(e.to_string().replace('\0', " ")).unwrap_or_default();
                    unsafe { error.write(msg.into_raw()) };
                }
                OAV_ERR_LOAD
            }
        }
    })
}

/// One document of a multi-document description.
#[repr(C)]
pub struct OavDocument {
    pub uri: *const c_char,
    pub uri_len: usize,
    pub data: *const u8,
    pub data_len: usize,
}

/// Like `oav_spec_load`, for a description spread over several documents:
/// `entry` is the OpenAPI document at `entry_uri`, and `others` are the
/// documents it refers to, each with the URI references use for it.
#[no_mangle]
pub unsafe extern "C" fn oav_spec_load_documents(
    entry_uri: *const c_char,
    entry_uri_len: usize,
    entry: *const u8,
    entry_len: usize,
    others: *const OavDocument,
    others_len: usize,
    opts: *const OavOptions,
    out: *mut *mut OavSpec,
    error: *mut *mut c_char,
) -> c_int {
    guard(|| {
        if out.is_null() {
            return OAV_ERR_NULL;
        }
        unsafe { out.write(std::ptr::null_mut()) };
        if !error.is_null() {
            unsafe { error.write(std::ptr::null_mut()) };
        }
        let uri = match unsafe { text(entry_uri, entry_uri_len) } {
            Ok(u) => u,
            Err(e) => return e,
        };
        let Some(entry) = (unsafe { bytes(entry, entry_len) }) else {
            return OAV_ERR_NULL;
        };
        let mut docs = Vec::with_capacity(others_len);
        if others_len > 0 {
            if others.is_null() {
                return OAV_ERR_NULL;
            }
            // SAFETY: the caller guarantees `others_len` valid documents.
            for d in unsafe { std::slice::from_raw_parts(others, others_len) } {
                let u = match unsafe { text(d.uri, d.uri_len) } {
                    Ok(u) => u,
                    Err(e) => return e,
                };
                let Some(b) = (unsafe { bytes(d.data, d.data_len) }) else {
                    return OAV_ERR_NULL;
                };
                docs.push((u, b));
            }
        }
        let options = if opts.is_null() {
            Options::default()
        } else {
            match to_options(unsafe { &*opts }) {
                Ok(o) => o,
                Err(e) => return e,
            }
        };
        match Spec::load_documents(uri, entry, &docs, options) {
            Ok(spec) => {
                unsafe { out.write(Box::into_raw(Box::new(OavSpec(spec)))) };
                OAV_OK
            }
            Err(e) => {
                if !error.is_null() {
                    let msg = CString::new(e.to_string().replace('\0', " ")).unwrap_or_default();
                    unsafe { error.write(msg.into_raw()) };
                }
                OAV_ERR_LOAD
            }
        }
    })
}

/// Frees a validator. Null is ignored.
#[no_mangle]
pub unsafe extern "C" fn oav_spec_free(spec: *mut OavSpec) {
    let _ = guard(|| {
        if !spec.is_null() {
            // SAFETY: created by `oav_spec_load` and not freed before.
            drop(unsafe { Box::from_raw(spec) });
        }
        OAV_OK
    });
}

struct OwnedReq {
    headers: Vec<(String, String)>,
}

unsafe fn with_request<R>(
    req: *const OavRequest,
    f: impl FnOnce(&Request<'_>) -> R,
) -> Result<R, c_int> {
    if req.is_null() {
        return Err(OAV_ERR_NULL);
    }
    // SAFETY: non-null and valid per the header.
    let r = unsafe { &*req };
    let method = unsafe { text(r.method, r.method_len) }?;
    let path = unsafe { text(r.path, r.path_len) }?;
    let query = unsafe { text(r.query, r.query_len) }?;
    let body = unsafe { bytes(r.body, r.body_len) }.ok_or(OAV_ERR_NULL)?;
    let owned = OwnedReq {
        headers: unsafe { headers(r.headers, r.headers_len) }?,
    };
    let hs: Vec<(&str, &str)> = owned
        .headers
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    Ok(f(&Request {
        method,
        path,
        query,
        headers: &hs,
        body,
    }))
}

/// Validates a request. On success `*out` owns a report to free with
/// `oav_report_free`.
#[no_mangle]
pub unsafe extern "C" fn oav_validate_request(
    spec: *const OavSpec,
    req: *const OavRequest,
    out: *mut *mut OavReport,
) -> c_int {
    guard(|| {
        if spec.is_null() || out.is_null() {
            return OAV_ERR_NULL;
        }
        unsafe { out.write(std::ptr::null_mut()) };
        // SAFETY: created by `oav_spec_load`.
        let spec = unsafe { &(*spec).0 };
        match unsafe { with_request(req, |r| spec.validate_request(r)) } {
            Ok(report) => {
                unsafe { out.write(Box::into_raw(Box::new(OavReport(report)))) };
                OAV_OK
            }
            Err(e) => e,
        }
    })
}

/// Validates a response to a request. On success `*out` owns a report to
/// free with `oav_report_free`.
#[no_mangle]
pub unsafe extern "C" fn oav_validate_response(
    spec: *const OavSpec,
    req: *const OavRequest,
    resp: *const OavResponse,
    out: *mut *mut OavReport,
) -> c_int {
    guard(|| {
        if spec.is_null() || out.is_null() || resp.is_null() {
            return OAV_ERR_NULL;
        }
        unsafe { out.write(std::ptr::null_mut()) };
        let spec = unsafe { &(*spec).0 };
        // SAFETY: non-null and valid per the header.
        let rs = unsafe { &*resp };
        let rh = match unsafe { headers(rs.headers, rs.headers_len) } {
            Ok(h) => h,
            Err(e) => return e,
        };
        let Some(body) = (unsafe { bytes(rs.body, rs.body_len) }) else {
            return OAV_ERR_NULL;
        };
        let rhs: Vec<(&str, &str)> = rh.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let response = Response {
            status: rs.status,
            headers: &rhs,
            body,
        };
        match unsafe { with_request(req, |r| spec.validate_response(r, &response)) } {
            Ok(report) => {
                unsafe { out.write(Box::into_raw(Box::new(OavReport(report)))) };
                OAV_OK
            }
            Err(e) => e,
        }
    })
}

/// 1 when the report has no problems, 0 otherwise (and for null).
#[no_mangle]
pub unsafe extern "C" fn oav_report_valid(report: *const OavReport) -> c_int {
    guard(|| {
        if report.is_null() {
            return 0;
        }
        c_int::from(unsafe { &(*report).0 }.is_valid())
    })
}

/// Number of problems kept in the report.
#[no_mangle]
pub unsafe extern "C" fn oav_report_len(report: *const OavReport) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        if report.is_null() {
            return 0;
        }
        unsafe { &(*report).0 }.problems().len()
    }))
    .unwrap_or(0)
}

/// Problems found but not kept ("and N more").
#[no_mangle]
pub unsafe extern "C" fn oav_report_omitted(report: *const OavReport) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        if report.is_null() {
            return 0;
        }
        unsafe { &(*report).0 }.omitted()
    }))
    .unwrap_or(0)
}

/// Reads problem `index`. The strings borrow from the report and stay valid
/// until `oav_report_free`.
#[no_mangle]
pub unsafe extern "C" fn oav_report_problem(
    report: *const OavReport,
    index: usize,
    out: *mut OavProblem,
) -> c_int {
    guard(|| {
        if report.is_null() || out.is_null() {
            return OAV_ERR_NULL;
        }
        let r = unsafe { &(*report).0 };
        let Some(p) = r.problems().get(index) else {
            return OAV_ERR_RANGE;
        };
        let kind: &'static [u8] = match p.kind.as_str() {
            "path_not_found" => b"path_not_found\0",
            "method_not_allowed" => b"method_not_allowed\0",
            "malformed_path" => b"malformed_path\0",
            "malformed_query" => b"malformed_query\0",
            "missing_parameter" => b"missing_parameter\0",
            "invalid_parameter" => b"invalid_parameter\0",
            "unknown_parameter" => b"unknown_parameter\0",
            "missing_credentials" => b"missing_credentials\0",
            "missing_body" => b"missing_body\0",
            "malformed_body" => b"malformed_body\0",
            "body_too_large" => b"body_too_large\0",
            "unsupported_media_type" => b"unsupported_media_type\0",
            "schema_violation" => b"schema_violation\0",
            "undeclared_status" => b"undeclared_status\0",
            "undeclared_operation" => b"undeclared_operation\0",
            "schema_error" => b"schema_error\0",
            _ => b"internal\0",
        };
        unsafe {
            out.write(OavProblem {
                kind: kind.as_ptr().cast(),
                location: p.location.as_ptr().cast(),
                location_len: p.location.len(),
                message: p.message.as_ptr().cast(),
                message_len: p.message.len(),
            })
        };
        OAV_OK
    })
}

/// The report as text, one problem per line, NUL-terminated. Free with
/// `oav_string_free`. Null on failure.
#[no_mangle]
pub unsafe extern "C" fn oav_report_to_string(report: *const OavReport) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        if report.is_null() {
            return std::ptr::null_mut();
        }
        let s = unsafe { &(*report).0 }.to_string().replace('\0', " ");
        CString::new(s).map_or(std::ptr::null_mut(), CString::into_raw)
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Frees a report. Null is ignored.
#[no_mangle]
pub unsafe extern "C" fn oav_report_free(report: *mut OavReport) {
    let _ = guard(|| {
        if !report.is_null() {
            // SAFETY: created by a validate call and not freed before.
            drop(unsafe { Box::from_raw(report) });
        }
        OAV_OK
    });
}

/// Frees a string returned by this library. Null is ignored.
#[no_mangle]
pub unsafe extern "C" fn oav_string_free(s: *mut c_char) {
    let _ = guard(|| {
        if !s.is_null() {
            // SAFETY: created by `CString::into_raw` in this library.
            drop(unsafe { CString::from_raw(s) });
        }
        OAV_OK
    });
}

/// The library version, static and NUL-terminated.
#[no_mangle]
pub extern "C" fn oav_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"},
      "paths": {"/p/{id}": {"get": {"parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}],
        "responses": {"200": {"description": "ok"}}}}}}"#;

    #[test]
    fn round_trip() {
        unsafe {
            let mut opts = std::mem::zeroed::<OavOptions>();
            assert_eq!(oav_options_default(&mut opts), OAV_OK);
            let mut spec = std::ptr::null_mut();
            assert_eq!(
                oav_spec_load(
                    DOC.as_ptr(),
                    DOC.len(),
                    &opts,
                    &mut spec,
                    std::ptr::null_mut()
                ),
                OAV_OK
            );
            let (m, p) = ("GET", "/p/x");
            let req = OavRequest {
                method: m.as_ptr().cast(),
                method_len: m.len(),
                path: p.as_ptr().cast(),
                path_len: p.len(),
                query: std::ptr::null(),
                query_len: 0,
                headers: std::ptr::null(),
                headers_len: 0,
                body: std::ptr::null(),
                body_len: 0,
            };
            let mut rep = std::ptr::null_mut();
            assert_eq!(oav_validate_request(spec, &req, &mut rep), OAV_OK);
            assert_eq!(oav_report_valid(rep), 0);
            assert_eq!(oav_report_len(rep), 1);
            let mut prob = std::mem::zeroed::<OavProblem>();
            assert_eq!(oav_report_problem(rep, 0, &mut prob), OAV_OK);
            let loc = std::slice::from_raw_parts(prob.location.cast::<u8>(), prob.location_len);
            assert_eq!(loc, b"path.id");
            assert_eq!(oav_report_problem(rep, 9, &mut prob), OAV_ERR_RANGE);
            let s = oav_report_to_string(rep);
            assert!(!s.is_null());
            oav_string_free(s);
            oav_report_free(rep);
            oav_spec_free(spec);
        }
    }

    #[test]
    fn errors_and_nulls() {
        unsafe {
            let mut spec = std::ptr::null_mut();
            let mut err = std::ptr::null_mut();
            assert_eq!(
                oav_spec_load(b"{".as_ptr(), 1, std::ptr::null(), &mut spec, &mut err),
                OAV_ERR_LOAD
            );
            assert!(spec.is_null());
            assert!(!err.is_null());
            oav_string_free(err);
            assert_eq!(
                oav_spec_load(
                    std::ptr::null(),
                    5,
                    std::ptr::null(),
                    &mut spec,
                    std::ptr::null_mut()
                ),
                OAV_ERR_NULL
            );
            assert_eq!(
                oav_validate_request(
                    std::ptr::null(),
                    std::ptr::null(),
                    &mut std::ptr::null_mut()
                ),
                OAV_ERR_NULL
            );
            oav_spec_free(std::ptr::null_mut());
            oav_report_free(std::ptr::null_mut());
            oav_string_free(std::ptr::null_mut());
            assert_eq!(oav_report_valid(std::ptr::null()), 0);
        }
    }
}
