struct BitonicParams {
    len: u32,
    k: u32,
    j: u32,
    ascend: u32,
}

impl Vec<T> {
    fn cas(self, left_idx: u32, right_idx: u32, direct: bool) {
        let left = self[left_idx];
        let right = self[right_idx];
        if (left > right && direct) || (left < right && !direct) {
            self[left_idx] = right;
            self[right_idx] = left;
        }
    }
}

pub fn main(params: BitonicParams, data: Vec<u32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();
    let i = group[0] * 256u32 + local[0];
    if i < params.len {
        let ixj = i ^ params.j;
        if ixj > i && ixj < params.len {
            let ascending_segment = (i & params.k) == 0u32;
            let compare_ascending = if params.ascend != 0u32 {
                ascending_segment
            } else {
                !ascending_segment
            };

            data.cas(i, ixj, compare_ascending);
        }
    }
}
