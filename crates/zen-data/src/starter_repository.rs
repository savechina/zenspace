pub struct StarterRepository {
    _db_path: String,
}

impl StarterRepository {
    pub fn new(db_path: String) -> Self {
        Self { _db_path: db_path }
    }
}
