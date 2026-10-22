package oav

import (
	"sync"
	"testing"
)

const doc = `
openapi: 3.1.0
info: {title: pets, version: "1"}
paths:
  /pets/{id}:
    get:
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer}}
        - {name: X-Trace, in: header, schema: {type: string, maxLength: 4}}
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema: {type: object, required: [name], properties: {name: {type: string}}}
`

func TestLoadAndValidate(t *testing.T) {
	s, err := Load([]byte(doc))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	r, err := s.ValidateRequest(&Request{Method: "GET", Path: "/pets/7", Headers: []Header{{"X-Trace", "abc"}}})
	if err != nil || !r.Valid() {
		t.Fatalf("valid request refused: %v %+v", err, r)
	}
	r, _ = s.ValidateRequest(&Request{Method: "GET", Path: "/pets/seven", Headers: []Header{{"X-Trace", "toolong"}}})
	if r.Valid() || len(r.Problems) != 2 {
		t.Fatalf("expected two problems, got %+v", r)
	}
	if r.Problems[0].Location != "path.id" || r.Problems[0].Kind != "invalid_parameter" {
		t.Fatalf("unexpected first problem %+v", r.Problems[0])
	}
	w, _ := s.ValidateResponse(&Request{Method: "GET", Path: "/pets/7"},
		&Response{Status: 200, Headers: []Header{{"Content-Type", "application/json"}}, Body: []byte(`{}`)})
	if w.Valid() || w.Problems[0].Location != "body/name" {
		t.Fatalf("expected body/name problem, got %+v", w)
	}
}

func TestLoadError(t *testing.T) {
	if _, err := Load([]byte("swagger: '1.2'")); err == nil {
		t.Fatal("expected a load error")
	}
}

func TestConcurrentUse(t *testing.T) {
	s, err := Load([]byte(doc))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	var wg sync.WaitGroup
	for i := 0; i < 16; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 200; j++ {
				r, err := s.ValidateRequest(&Request{Method: "GET", Path: "/pets/1"})
				if err != nil || !r.Valid() {
					t.Errorf("unexpected %v %+v", err, r)
					return
				}
			}
		}()
	}
	wg.Wait()
	if Version() == "" {
		t.Fatal("empty version")
	}
}

func TestLoadDocuments(t *testing.T) {
	entry := []byte("openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /p/{id}:\n    $ref: paths.yaml#/item\n")
	paths := []byte("item:\n  get:\n    parameters:\n      - {name: id, in: path, required: true, schema: {type: integer}}\n    responses: {'200': {description: ok}}\n")
	s, err := LoadDocuments("https://x.example/openapi.yaml", entry, []Document{{URI: "paths.yaml", Data: paths}})
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	r, _ := s.ValidateRequest(&Request{Method: "GET", Path: "/p/x"})
	if r.Valid() || r.Problems[0].Location != "path.id" {
		t.Fatalf("expected path.id problem, got %+v", r)
	}
	if _, err := LoadDocuments("https://x.example/openapi.yaml", entry, nil); err == nil {
		t.Fatal("expected a missing-document error")
	}
}

const hooks = `
openapi: 3.1.0
info: {title: hooks, version: "1"}
paths:
  /subs:
    post:
      operationId: subscribe
      responses: {"201": {description: ok}}
      callbacks:
        onEvent:
          "{$request.body#/url}":
            post:
              requestBody:
                required: true
                content:
                  application/json:
                    schema: {type: object, required: [event]}
              responses: {"204": {description: ok}}
webhooks:
  ping:
    post:
      responses: {"200": {description: ok}}
`

func TestWebhooksAndCallbacks(t *testing.T) {
	s, err := Load([]byte(hooks))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	post := func(body string) *Request {
		return &Request{Method: "POST", Path: "/", Headers: []Header{{"Content-Type", "application/json"}}, Body: []byte(body)}
	}
	if r, err := s.ValidateWebhookRequest("ping", &Request{Method: "POST", Path: "/"}); err != nil || !r.Valid() {
		t.Fatalf("webhook request refused: %v %+v", err, r)
	}
	if r, _ := s.ValidateWebhookRequest("pong", &Request{Method: "POST", Path: "/"}); r.Valid() || r.Problems[0].Kind != "undeclared_operation" {
		t.Fatalf("unknown webhook accepted: %+v", r)
	}
	if r, _ := s.ValidateWebhookResponse("ping", &Request{Method: "POST", Path: "/"}, &Response{Status: 500}); r.Valid() {
		t.Fatalf("undeclared status accepted: %+v", r)
	}
	if r, err := s.ValidateCallbackRequest("subscribe", "onEvent", post(`{"event": "x"}`)); err != nil || !r.Valid() {
		t.Fatalf("callback request refused: %v %+v", err, r)
	}
	if r, _ := s.ValidateCallbackRequest("subscribe", "onEvent", post(`{}`)); r.Valid() {
		t.Fatalf("invalid callback body accepted: %+v", r)
	}
	if r, err := s.ValidateCallbackResponse("subscribe", "onEvent", post(`{}`), &Response{Status: 204}); err != nil || !r.Valid() {
		t.Fatalf("callback response refused: %v %+v", err, r)
	}
}
