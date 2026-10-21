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
	if _, err := Load([]byte("swagger: '2.0'")); err == nil {
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
