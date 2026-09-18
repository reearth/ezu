package ezu

// Rebuilding the embedded module:
//
//	go generate ./...
//
// It is a committed artefact rather than something built at `go get` time,
// so that using this package needs no Rust toolchain. The cost is that it
// can go stale, and three things catch that: the ABI version is checked
// when a renderer is instantiated, a Rust test compares the committed
// module's export list with what the source builds, and CI runs these tests
// a second time against a freshly built one. See .github/workflows/ci.yml,
// which says which catches what.
//
// The recipe lives in scripts/build-wasm-go.sh rather than in the line
// below, because the choices in it — opt-level 3 rather than a size
// profile, +simd128, wasm-opt -Oz, no strip — are measured ones that
// deserve the room to say why.

//go:generate sh -c "cd .. && ./scripts/build-wasm-go.sh"
