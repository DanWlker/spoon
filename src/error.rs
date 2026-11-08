// An error of any kind, boxed: reading the config, choosing the graph cells, opening the
// repository.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
