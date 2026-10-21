// Package oav is a thin cgo wrapper over the openapi-validator C ABI.
//
// Build the static library first:
//
//	cargo build --release -p openapi-validator-ffi
package oav

/*
#cgo CFLAGS: -I${SRCDIR}/../include
#cgo LDFLAGS: ${SRCDIR}/../../target/release/libopenapi_validator_ffi.a
#cgo linux LDFLAGS: -lm -ldl -lpthread
#cgo darwin LDFLAGS: -framework CoreFoundation
#include <stdlib.h>
#include "openapi_validator.h"
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"unsafe"
)

// Spec is a loaded document. It is safe for concurrent use.
type Spec struct{ p *C.oav_spec }

// Header is one header line.
type Header struct{ Name, Value string }

// Request describes an HTTP request as received.
type Request struct {
	Method, Path, Query string
	Headers             []Header
	Body                []byte
}

// Response describes an upstream response.
type Response struct {
	Status  uint16
	Headers []Header
	Body    []byte
}

// Problem is one finding.
type Problem struct{ Kind, Location, Message string }

// Report lists the problems found; Omitted counts those past the bounds.
type Report struct {
	Problems []Problem
	Omitted  int
}

// Valid reports whether nothing was found.
func (r Report) Valid() bool { return len(r.Problems) == 0 && r.Omitted == 0 }

// Load parses and indexes a document with default options.
func Load(doc []byte) (*Spec, error) {
	var opts C.oav_options
	if rc := C.oav_options_default(&opts); rc != C.OAV_OK {
		return nil, fmt.Errorf("oav_options_default: %d", int(rc))
	}
	var p *C.oav_spec
	var cerr *C.char
	var docPtr *C.uint8_t
	if len(doc) > 0 {
		docPtr = (*C.uint8_t)(unsafe.Pointer(&doc[0]))
	}
	rc := C.oav_spec_load(docPtr, C.size_t(len(doc)), &opts, &p, &cerr)
	runtime.KeepAlive(doc)
	if rc != C.OAV_OK {
		msg := fmt.Sprintf("load failed (%d)", int(rc))
		if cerr != nil {
			msg = C.GoString(cerr)
			C.oav_string_free(cerr)
		}
		return nil, errors.New(msg)
	}
	s := &Spec{p: p}
	runtime.SetFinalizer(s, func(s *Spec) { s.Close() })
	return s, nil
}

// Document is one document of a multi-document description.
type Document struct {
	URI  string
	Data []byte
}

// LoadDocuments loads a description spread over several documents: entry
// is the OpenAPI document at entryURI, and others are the documents it
// refers to. Nothing is fetched.
func LoadDocuments(entryURI string, entry []byte, others []Document) (*Spec, error) {
	var opts C.oav_options
	if rc := C.oav_options_default(&opts); rc != C.OAV_OK {
		return nil, fmt.Errorf("oav_options_default: %d", int(rc))
	}
	var pin runtime.Pinner
	defer pin.Unpin()
	uri, uriLen := cstr(&pin, entryURI)
	data, dataLen := cbytes(&pin, entry)
	var docs *C.oav_document
	if len(others) > 0 {
		size := C.size_t(len(others)) * C.size_t(unsafe.Sizeof(C.oav_document{}))
		docs = (*C.oav_document)(C.malloc(size))
		defer C.free(unsafe.Pointer(docs))
		view := unsafe.Slice(docs, len(others))
		for i, d := range others {
			view[i].uri, view[i].uri_len = cstr(&pin, d.URI)
			view[i].data, view[i].data_len = cbytes(&pin, d.Data)
		}
	}
	var p *C.oav_spec
	var cerr *C.char
	rc := C.oav_spec_load_documents(uri, uriLen, data, dataLen, docs, C.size_t(len(others)), &opts, &p, &cerr)
	if rc != C.OAV_OK {
		msg := fmt.Sprintf("load failed (%d)", int(rc))
		if cerr != nil {
			msg = C.GoString(cerr)
			C.oav_string_free(cerr)
		}
		return nil, errors.New(msg)
	}
	s := &Spec{p: p}
	runtime.SetFinalizer(s, func(s *Spec) { s.Close() })
	return s, nil
}

// Close frees the validator. It is safe to call more than once.
func (s *Spec) Close() {
	if s.p != nil {
		C.oav_spec_free(s.p)
		s.p = nil
	}
}

// cstr returns a pointer to a string's bytes, pinned for the call.
func cstr(pin *runtime.Pinner, s string) (*C.char, C.size_t) {
	if len(s) == 0 {
		return nil, 0
	}
	p := unsafe.StringData(s)
	pin.Pin(p)
	return (*C.char)(unsafe.Pointer(p)), C.size_t(len(s))
}

func cbytes(pin *runtime.Pinner, b []byte) (*C.uint8_t, C.size_t) {
	if len(b) == 0 {
		return nil, 0
	}
	pin.Pin(&b[0])
	return (*C.uint8_t)(unsafe.Pointer(&b[0])), C.size_t(len(b))
}

// headers builds a C array of oav_header in C memory; free with C.free.
func headers(pin *runtime.Pinner, hs []Header) (*C.oav_header, C.size_t) {
	if len(hs) == 0 {
		return nil, 0
	}
	size := C.size_t(len(hs)) * C.size_t(unsafe.Sizeof(C.oav_header{}))
	arr := (*C.oav_header)(C.malloc(size))
	view := unsafe.Slice(arr, len(hs))
	for i, h := range hs {
		view[i].name, view[i].name_len = cstr(pin, h.Name)
		view[i].value, view[i].value_len = cstr(pin, h.Value)
	}
	return arr, C.size_t(len(hs))
}

func (s *Spec) cRequest(pin *runtime.Pinner, r *Request) (C.oav_request, func()) {
	var req C.oav_request
	req.method, req.method_len = cstr(pin, r.Method)
	req.path, req.path_len = cstr(pin, r.Path)
	req.query, req.query_len = cstr(pin, r.Query)
	req.body, req.body_len = cbytes(pin, r.Body)
	req.headers, req.headers_len = headers(pin, r.Headers)
	return req, func() {
		if req.headers != nil {
			C.free(unsafe.Pointer(req.headers))
		}
	}
}

func readReport(rep *C.oav_report) Report {
	defer C.oav_report_free(rep)
	n := int(C.oav_report_len(rep))
	out := Report{Omitted: int(C.oav_report_omitted(rep))}
	for i := 0; i < n; i++ {
		var p C.oav_problem
		if C.oav_report_problem(rep, C.size_t(i), &p) != C.OAV_OK {
			continue
		}
		out.Problems = append(out.Problems, Problem{
			Kind:     C.GoString(p.kind),
			Location: C.GoStringN(p.location, C.int(p.location_len)),
			Message:  C.GoStringN(p.message, C.int(p.message_len)),
		})
	}
	return out
}

// ValidateRequest checks a request.
func (s *Spec) ValidateRequest(r *Request) (Report, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	return s.request(&pin, r, "oav_validate_request",
		func(req *C.oav_request, rep **C.oav_report) C.int {
			return C.oav_validate_request(s.p, req, rep)
		})
}

// ValidateResponse checks a response to a request.
func (s *Spec) ValidateResponse(r *Request, w *Response) (Report, error) {
	return s.response(r, w, "oav_validate_response",
		func(req *C.oav_request, resp *C.oav_response, rep **C.oav_report) C.int {
			return C.oav_validate_response(s.p, req, resp, rep)
		})
}

// ValidateWebhookRequest checks a request sent to the webhook name. The
// method selects the operation; the path is not checked.
func (s *Spec) ValidateWebhookRequest(name string, r *Request) (Report, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	n, nl := cstr(&pin, name)
	return s.request(&pin, r, "oav_validate_webhook_request",
		func(req *C.oav_request, rep **C.oav_report) C.int {
			return C.oav_validate_webhook_request(s.p, n, nl, req, rep)
		})
}

// ValidateWebhookResponse checks the response to a webhook request.
func (s *Spec) ValidateWebhookResponse(name string, r *Request, w *Response) (Report, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	n, nl := cstr(&pin, name)
	return s.response(r, w, "oav_validate_webhook_response",
		func(req *C.oav_request, resp *C.oav_response, rep **C.oav_report) C.int {
			return C.oav_validate_webhook_response(s.p, n, nl, req, resp, rep)
		})
}

// ValidateCallbackRequest checks a request sent for the callback named
// callback of the operation operationID.
func (s *Spec) ValidateCallbackRequest(operationID, callback string, r *Request) (Report, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	o, ol := cstr(&pin, operationID)
	c, cl := cstr(&pin, callback)
	return s.request(&pin, r, "oav_validate_callback_request",
		func(req *C.oav_request, rep **C.oav_report) C.int {
			return C.oav_validate_callback_request(s.p, o, ol, c, cl, req, rep)
		})
}

// ValidateCallbackResponse checks the response to a callback request.
func (s *Spec) ValidateCallbackResponse(operationID, callback string, r *Request, w *Response) (Report, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	o, ol := cstr(&pin, operationID)
	c, cl := cstr(&pin, callback)
	return s.response(r, w, "oav_validate_callback_response",
		func(req *C.oav_request, resp *C.oav_response, rep **C.oav_report) C.int {
			return C.oav_validate_callback_response(s.p, o, ol, c, cl, req, resp, rep)
		})
}

func (s *Spec) request(pin *runtime.Pinner, r *Request, name string,
	call func(*C.oav_request, **C.oav_report) C.int) (Report, error) {
	req, free := s.cRequest(pin, r)
	defer free()
	var rep *C.oav_report
	if rc := call(&req, &rep); rc != C.OAV_OK {
		return Report{}, fmt.Errorf("%s: %d", name, int(rc))
	}
	return readReport(rep), nil
}

func (s *Spec) response(r *Request, w *Response, name string,
	call func(*C.oav_request, *C.oav_response, **C.oav_report) C.int) (Report, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	req, free := s.cRequest(&pin, r)
	defer free()
	var resp C.oav_response
	resp.status = C.uint16_t(w.Status)
	resp.body, resp.body_len = cbytes(&pin, w.Body)
	resp.headers, resp.headers_len = headers(&pin, w.Headers)
	defer func() {
		if resp.headers != nil {
			C.free(unsafe.Pointer(resp.headers))
		}
	}()
	var rep *C.oav_report
	if rc := call(&req, &resp, &rep); rc != C.OAV_OK {
		return Report{}, fmt.Errorf("%s: %d", name, int(rc))
	}
	return readReport(rep), nil
}

// Version returns the library version.
func Version() string { return C.GoString(C.oav_version()) }
