pub use ffi::*;

#[allow(clippy::module_inception)]
#[allow(clippy::too_many_arguments)]
#[cxx::bridge]
mod ffi {
    #[namespace = "GeoNLP"]
    unsafe extern "C++" {
        include!("geocoder_nlp/geocoder-nlp/src/geocoder.h");

        type Geocoder;

        fn new_geocoder() -> UniquePtr<Geocoder>;
        fn set_max_results(self: Pin<&mut Geocoder>, max_results: u64);
        fn get_max_results(self: &Geocoder) -> u64;
        fn load(self: Pin<&mut Geocoder>, dbname: &CxxString) -> bool;
        fn search(
            self: Pin<&mut Geocoder>,
            parsed_query: &CxxVector<ParseResult>,
            result: Pin<&mut CxxVector<GeoResult>>,
            min_levels: u64,
            reference: &GeoReference,
        ) -> bool;
        fn search_nearby(
            self: Pin<&mut Geocoder>,
            name_query: &CxxVector<CxxString>,
            type_query: &CxxVector<CxxString>,
            latitude: f64,
            longitude: f64,
            radius: f64,
            result: Pin<&mut CxxVector<GeoResult>>,
            postal: Pin<&mut Postal>,
        ) -> bool;

        type GeoReference;

        fn new_geo_reference(
            lat: f64,
            lon: f64,
            zoom: i32,
            importance: f64,
        ) -> UniquePtr<GeoReference>;
        fn empty_geo_reference() -> UniquePtr<GeoReference>;

        type GeoResult;

        fn get_latitude(self: &GeoResult) -> f64;
        fn get_longitude(self: &GeoResult) -> f64;
        fn get_distance(self: &GeoResult) -> f64;
        fn get_search_rank(self: &GeoResult) -> f64;
        fn get_title(self: &GeoResult) -> &CxxString;
        fn get_address(self: &GeoResult) -> &CxxString;
        fn get_type(self: &GeoResult) -> &CxxString;
        fn get_phone(self: &GeoResult) -> &CxxString;
        fn get_postal_code(self: &GeoResult) -> &CxxString;
        fn get_website(self: &GeoResult) -> &CxxString;
    }

    #[namespace = "GeoNLP"]
    unsafe extern "C++" {
        include!("geocoder_nlp/geocoder-nlp/src/postal.h");

        type Postal;

        fn new_postal() -> UniquePtr<Postal>;
        fn set_postal_datadir(self: Pin<&mut Postal>, global: &CxxString, country: &CxxString);
        fn set_postal_datadir_country(self: Pin<&mut Postal>, country: &CxxString);
        fn set_use_primitive(self: Pin<&mut Postal>, primitive: bool);
        fn parse(
            self: Pin<&mut Postal>,
            input: &CxxString,
            output: Pin<&mut CxxVector<ParseResult>>,
            nonormalization: Pin<&mut ParseResult>,
        ) -> bool;

        type ParseResult;

        fn new_parse_result() -> UniquePtr<ParseResult>;
    }
}
