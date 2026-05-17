pub const IMPORTED_CONST: i32 = 9i32;

pub struct ImportedPair {
    left: i32,
    right: i32,
}

impl ImportedPair {
    pub fn sum(self: ImportedPair) {
        self.left + self.right
    }
}

pub fn imported_add(left: i32, right: i32) {
    left + right
}

