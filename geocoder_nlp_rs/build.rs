use std::path::Path;
use std::{env, fs};

use cc::Build;
use pkg_config::Config as PkgConfig;

fn main() {
    // Find and link dynamically against common geocoder-nlp dependencies.
    let kyotocabinet = PkgConfig::new().atleast_version("1.0.0").probe("kyotocabinet").unwrap();
    let sqlite3 = PkgConfig::new().atleast_version("3.0.0").probe("sqlite3").unwrap();
    let marisa = PkgConfig::new().atleast_version("0.2.0").probe("marisa").unwrap();

    // Collect include paths, so cxx_build can find them when cross-compiling.
    let mut include_paths = kyotocabinet.include_paths;
    include_paths.extend(sqlite3.include_paths);
    include_paths.extend(marisa.include_paths);
    include_paths.sort_unstable();
    include_paths.dedup();

    // Compile libpostal for static linking, since it's usually not packaged.

    let mut build = Build::new();
    build.define("LIBPOSTAL_DATA_DIR", "\"/usr/local/share/libpostal\"");
    build.define("HAVE__BOOL", "1");
    build.define("HAVE_DIRENT_H", "1");
    build.define("HAVE_DLFCN_H", "1");
    build.define("HAVE_DRAND48", "1");
    build.define("HAVE_FCNTL_H", "1");
    build.define("HAVE_FLOAT_H", "1");
    build.define("HAVE_GETCWD", "1");
    build.define("HAVE_GETTIMEOFDAY", "1");
    build.define("HAVE_INTTYPES_H", "1");
    build.define("HAVE_LIMITS_H", "1");
    build.define("HAVE_LOCALE_H", "1");
    build.define("HAVE_MALLOC", "1");
    build.define("HAVE_MALLOC_H", "1");
    build.define("HAVE_MEMMOVE", "1");
    build.define("HAVE_MEMORY_H", "1");
    build.define("HAVE_MEMSET", "1");
    build.define("HAVE_PTRDIFF_T", "1");
    build.define("HAVE_REALLOC", "1");
    build.define("HAVE_REGCOMP", "1");
    build.define("HAVE_SETLOCALE", "1");
    build.define("HAVE_SHUF", "1");
    build.define("HAVE_SQRT", "1");
    build.define("HAVE_STDBOOL_H", "1");
    build.define("HAVE_STDDEF_H", "1");
    build.define("HAVE_STDINT_H", "1");
    build.define("HAVE_STDIO_H", "1");
    build.define("HAVE_STDLIB_H", "1");
    build.define("HAVE_STRDUP", "1");
    build.define("HAVE_STRING_H", "1");
    build.define("HAVE_STRINGS_H", "1");
    build.define("HAVE_STRNDUP", "1");
    build.define("HAVE_SYS_STAT_H", "1");
    build.define("HAVE_SYS_TIME_H", "1");
    build.define("HAVE_SYS_TYPES_H", "1");
    build.define("HAVE_UNISTD_H", "1");
    build.file("libpostal/src/acronyms.c");
    build.file("libpostal/src/address_dictionary.c");
    build.file("libpostal/src/address_parser.c");
    build.file("libpostal/src/address_parser_io.c");
    build.file("libpostal/src/averaged_perceptron.c");
    build.file("libpostal/src/averaged_perceptron_tagger.c");
    build.file("libpostal/src/crf.c");
    build.file("libpostal/src/crf_context.c");
    build.file("libpostal/src/dedupe.c");
    build.file("libpostal/src/double_metaphone.c");
    build.file("libpostal/src/expand.c");
    build.file("libpostal/src/features.c");
    build.file("libpostal/src/file_utils.c");
    build.file("libpostal/src/float_utils.c");
    build.file("libpostal/src/geohash/geohash.c");
    build.file("libpostal/src/graph_builder.c");
    build.file("libpostal/src/graph.c");
    build.file("libpostal/src/jaccard.c");
    build.file("libpostal/src/language_classifier.c");
    build.file("libpostal/src/language_features.c");
    build.file("libpostal/src/libpostal.c");
    build.file("libpostal/src/logistic.c");
    build.file("libpostal/src/logistic_regression.c");
    build.file("libpostal/src/minibatch.c");
    build.file("libpostal/src/near_dupe.c");
    build.file("libpostal/src/ngrams.c");
    build.file("libpostal/src/normalize.c");
    build.file("libpostal/src/numex.c");
    build.file("libpostal/src/place.c");
    build.file("libpostal/src/scanner.c");
    build.file("libpostal/src/soft_tfidf.c");
    build.file("libpostal/src/sparse_matrix.c");
    build.file("libpostal/src/string_similarity.c");
    build.file("libpostal/src/string_utils.c");
    build.file("libpostal/src/strndup.c");
    build.file("libpostal/src/tokens.c");
    build.file("libpostal/src/transliterate.c");
    build.file("libpostal/src/trie.c");
    build.file("libpostal/src/trie_search.c");
    build.file("libpostal/src/trie_utils.c");
    build.file("libpostal/src/unicode_scripts.c");
    build.file("libpostal/src/utf8proc/utf8proc.c");
    build.cargo_warnings(false);
    build.compile("postal");
    println!("cargo:rerun-if-changed=libpostal/src/");

    // Copy libpostal header so it matches geocoder-nlp import.
    let out_dir = env::var("OUT_DIR").unwrap();
    let dir = Path::new(&out_dir).join("libpostal");
    fs::create_dir_all(&dir).unwrap();
    fs::copy("libpostal/src/libpostal.h", dir.join("libpostal.h")).unwrap();

    // Compile geocoder-nlp and its Rust bindings.
    cxx_build::bridge("src/ffi.rs")
        .file("geocoder-nlp/src/geocoder.cpp")
        .file("geocoder-nlp/src/postal.cpp")
        .include("geocoder-nlp/thirdparty/sqlite3pp/headeronly_src/")
        .includes(include_paths)
        .include(out_dir)
        .std("c++14")
        // Cross-compiling for aarch64 emits a lot of warnings,
        // so to avoid cluttering the build output we silence them.
        .warnings(false)
        .compile("geocoder-nlp");
    println!("cargo:rerun-if-changed=geocoder-nlp/src/");
}
