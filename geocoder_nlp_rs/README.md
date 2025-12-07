# Geocoder NLP Rust bindings

Geocoding is the process of taking a text-based description of a location, such
as an address or the name of a place, and returning geographic coordinates.

This crate statically compiles [geocoder-nlp], which is an offline-capable
geocoder using [postal] for address normalization.

While geocoder-nlp and libpostal are compiled statically, it will still
link dynamically to kyotocabinet, sqlite3, and marisa. All of these are
required runtime dependencies.

Additionally boost is required as a compile-time dependency.

[geocoder-nlp]: https://github.com/rinigus/geocoder-nlp
[postal]: https://github.com/openvenues/libpostal

## Examples

```rs
use geocoder_nlp::Geocoder;

let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();

// Get all results matching `Rúa` in our selected dataset.
let mut results = geocoder.search("Rúa", None).unwrap();

// Output results in descending relevance.
while let Some(result) = results.next() {
    println!("Title: {}", result.title());
    println!("Latitude: {}, Longitude: {}", result.latitude(), result.longitude());
    println!("Address: {} {}", result.postal_code(), result.address());
    println!();
}
```
