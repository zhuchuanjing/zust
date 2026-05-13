pub struct BigFloat<N> {
    sign: bool,
    exp: i32,
    data: [u32; N],
}

pub fn bf_add_carry(a: u32, b: u32) {
    let sum = a + b;
    let carry = if sum < a { 1u32 } else { 0u32 };
    [sum, carry] as [u32; 2]
}

pub fn bf_sub_borrow(a: u32, b: u32, borrow: u32) {
    let av = a as u64;
    let bv = (b as u64) + (borrow as u64);
    if av >= bv {
        [(av - bv) as u32, 0u32] as [u32; 2]
    } else {
        [(4294967296u64 + av - bv) as u32, 1u32] as [u32; 2]
    }
}

pub fn bf_mul_u32_wide(a: u32, b: u32) {
    let mask = 65535u32;

    let a0 = a & mask;
    let a1 = a >> 16u32;
    let b0 = b & mask;
    let b1 = b >> 16u32;

    let p0 = a0 * b0;
    let p1 = a0 * b1;
    let p2 = a1 * b0;
    let p3 = a1 * b1;

    let r1 = bf_add_carry(p0, (p1 & mask) << 16u32);
    let lo1 = r1[0];
    let hi1 = p3 + (p1 >> 16u32) + r1[1];

    let r2 = bf_add_carry(lo1, (p2 & mask) << 16u32);
    let lo = r2[0];
    let hi = hi1 + (p2 >> 16u32) + r2[1];

    [lo, hi] as [u32; 2]
}

pub fn bf_base_f32() {
    4294967296.0f32
}

pub fn bf_mod_pow(base: u64, exp: u64, modu: u64) {
    let result = 1u64;
    base = base % modu;
    while exp > 0u64 {
        if (exp & 1u64) != 0u64 {
            result = (result * base) % modu;
        }
        base = (base * base) % modu;
        exp = exp >> 1u64;
    }
    result
}

pub struct BfScratch<M> {
    data: [u64; M],
}

impl BfScratch<M> {
    pub fn zero() {
        let out: [u64; M] = [0u64; M];
        for idx in 0..M {
            out[idx] = 0u64;
        }
        BfScratch<M>{ data: out }
    }

    pub fn ntt(self: BfScratch<M>, invert: bool, modu: u64, primitive_root: u64) {
        let out: [u64; M] = self.data;

        let i = 1;
        let j = 0;
        while i < M {
            let bit = M >> 1;
            while (j & bit) != 0 {
                j = j ^ bit;
                bit = bit >> 1;
            }
            j = j ^ bit;
            if i < j {
                let tmp = out[i];
                out[i] = out[j];
                out[j] = tmp;
            }
            i += 1;
        }

        let len = 2;
        while len <= M {
            let wlen = if invert {
                bf_mod_pow(primitive_root, (modu - 1u64) - ((modu - 1u64) / (len as u64)), modu)
            } else {
                bf_mod_pow(primitive_root, (modu - 1u64) / (len as u64), modu)
            };

            let block = 0;
            while block < M {
                let w = 1u64;
                let half = len >> 1;
                for offset in 0..half {
                    let u = out[block + offset];
                    let v = (out[block + offset + half] * w) % modu;
                    out[block + offset] = (u + v) % modu;
                    if u >= v {
                        out[block + offset + half] = u - v;
                    } else {
                        out[block + offset + half] = u + modu - v;
                    }
                    w = (w * wlen) % modu;
                }
                block += len;
            }
            len = len << 1;
        }

        if invert {
            let inv_m = bf_mod_pow(M as u64, modu - 2u64, modu);
            for idx in 0..M {
                out[idx] = (out[idx] * inv_m) % modu;
            }
        }

        BfScratch<M>{ data: out }
    }

    pub fn pointwise(self: BfScratch<M>, rhs: BfScratch<M>, modu: u64) {
        let out: [u64; M] = self.data;
        for idx in 0..M {
            out[idx] = (out[idx] * rhs.data[idx]) % modu;
        }
        BfScratch<M>{ data: out }
    }
}

impl BigFloat<N> {
    pub fn zero() {
        let out: [u32; N] = [0u32; N];
        for idx in 0..N {
            out[idx] = 0u32;
        }
        BigFloat<N>{
            sign: false,
            exp: 0i32,
            data: out,
        }
    }

    pub fn from_u32(value: u32) {
        let out: [u32; N] = [0u32; N];
        for idx in 0..N {
            out[idx] = 0u32;
        }

        if value == 0u32 {
            BigFloat<N>{
                sign: false,
                exp: 0i32,
                data: out,
            }
        } else {
            out[N - 1] = value;
            BigFloat<N>{
                sign: false,
                exp: 0i32 - ((N - 1) as i32),
                data: out,
            }
        }
    }

    pub fn from_f32(value: f32) {
        let out: [u32; N] = [0u32; N];
        for idx in 0..N {
            out[idx] = 0u32;
        }

        let sign = value < 0.0f32;
        let mag = if sign { -value } else { value };
        if mag == 0.0f32 || mag != mag {
            BigFloat<N>{
                sign: false,
                exp: 0i32,
                data: out,
            }
        } else {
            let base = bf_base_f32();
            let exp = 0i32;

            let norm_steps = 0;
            while mag >= base && norm_steps < 16 {
                mag = mag / base;
                exp += 1;
                norm_steps += 1;
            }

            let tiny_steps = 0;
            while mag < 1.0f32 && tiny_steps < 16 {
                mag = mag * base;
                exp -= 1;
                tiny_steps += 1;
            }

            if mag >= base {
                for idx in 0..N {
                    out[idx] = 4294967295u32;
                }

                BigFloat<N>{
                    sign: sign,
                    exp: exp,
                    data: out,
                }
            } else if mag < 1.0f32 {
                BigFloat<N>{
                    sign: false,
                    exp: 0i32,
                    data: out,
                }
            } else {
                let idx = N;
                while idx > 0 {
                    idx -= 1;
                    let limb = mag as u32;
                    out[idx] = limb;
                    mag = (mag - (limb as f32)) * base;
                    if idx > 0 {
                        exp -= 1;
                    }
                }

                BigFloat<N>{
                    sign: sign,
                    exp: exp,
                    data: out,
                }
            }
        }
    }

    pub fn to_f32(self: BigFloat<N>) {
        let base = bf_base_f32();
        let value = 0.0f32;

        for idx in 0..N {
            let limb = self.data[idx];
            if limb != 0u32 {
                let item = limb as f32;
                let power = self.exp + (idx as i32);
                let steps = 0;
                while power > 0 && steps < 16 {
                    item = item * base;
                    power -= 1;
                    steps += 1;
                }
                if power > 0 {
                    item = item * base;
                }

                steps = 0;
                while power < 0 && steps < 16 {
                    item = item / base;
                    power += 1;
                    steps += 1;
                }
                if power == 0 {
                    value = value + item;
                }
            }
        }

        if self.sign {
            -value
        } else {
            value
        }
    }

    fn is_zero(self: BigFloat<N>) {
        let has_limb = false;
        for idx in 0..N {
            if self.data[idx] != 0u32 {
                has_limb = true;
            }
        }
        !has_limb
    }

    fn abs_cmp(self: BigFloat<N>, rhs: BigFloat<N>) {
        let self_high = self.exp + ((N - 1) as i32);
        let rhs_high = rhs.exp + ((N - 1) as i32);
        let high = if self_high >= rhs_high { self_high } else { rhs_high };
        let low = if self.exp <= rhs.exp { self.exp } else { rhs.exp };
        let result = 0i32;
        let power = high;

        while power >= low && result == 0i32 {
            let a_idx = power - self.exp;
            let b_idx = power - rhs.exp;
            let a_limb = 0u32;
            let b_limb = 0u32;

            if a_idx >= 0i32 && a_idx < (N as i32) {
                a_limb = self.data[a_idx as u32];
            }
            if b_idx >= 0i32 && b_idx < (N as i32) {
                b_limb = rhs.data[b_idx as u32];
            }

            if a_limb > b_limb {
                result = 1i32;
            } else if a_limb < b_limb {
                result = -1i32;
            }

            power -= 1i32;
        }

        result
    }

    pub fn cmp(self: BigFloat<N>, rhs: BigFloat<N>) {
        if self.is_zero() && rhs.is_zero() {
            0i32
        } else if self.sign != rhs.sign {
            if self.sign { -1i32 } else { 1i32 }
        } else {
            let cmp = self.abs_cmp(rhs);
            if self.sign { -cmp } else { cmp }
        }
    }

    pub fn eq(self: BigFloat<N>, rhs: BigFloat<N>) {
        self.cmp(rhs) == 0i32
    }

    pub fn ne(self: BigFloat<N>, rhs: BigFloat<N>) {
        self.cmp(rhs) != 0i32
    }

    pub fn lt(self: BigFloat<N>, rhs: BigFloat<N>) {
        self.cmp(rhs) < 0i32
    }

    pub fn le(self: BigFloat<N>, rhs: BigFloat<N>) {
        self.cmp(rhs) <= 0i32
    }

    pub fn gt(self: BigFloat<N>, rhs: BigFloat<N>) {
        self.cmp(rhs) > 0i32
    }

    pub fn ge(self: BigFloat<N>, rhs: BigFloat<N>) {
        self.cmp(rhs) >= 0i32
    }

    fn add_abs(self: BigFloat<N>, rhs: BigFloat<N>, result_sign: bool) {
        let wide: [u32; N + 1] = [0u32; N + 1];
        let out: [u32; N] = [0u32; N];

        for idx in 0..(N + 1) {
            wide[idx] = 0u32;
        }
        for idx in 0..N {
            out[idx] = 0u32;
        }

        let exp = if self.exp >= rhs.exp { self.exp } else { rhs.exp };

        for idx in 0..N {
            let target = (idx as i32) + self.exp - exp;
            if target >= 0i32 && target < ((N + 1) as i32) {
                let pos = target as u32;
                let added = bf_add_carry(wide[pos], self.data[idx]);
                wide[pos] = added[0];
                let carry = added[1];
                let k = pos + 1;
                while carry != 0u32 && k < (N + 1) {
                    let next = bf_add_carry(wide[k], carry);
                    wide[k] = next[0];
                    carry = next[1];
                    k += 1;
                }
            }
        }

        for idx in 0..N {
            let target = (idx as i32) + rhs.exp - exp;
            if target >= 0i32 && target < ((N + 1) as i32) {
                let pos = target as u32;
                let added = bf_add_carry(wide[pos], rhs.data[idx]);
                wide[pos] = added[0];
                let carry = added[1];
                let k = pos + 1;
                while carry != 0u32 && k < (N + 1) {
                    let next = bf_add_carry(wide[k], carry);
                    wide[k] = next[0];
                    carry = next[1];
                    k += 1;
                }
            }
        }

        if wide[N] != 0u32 {
            for idx in 0..N {
                out[idx] = wide[idx + 1];
            }
            exp += 1i32;
        } else {
            for idx in 0..N {
                out[idx] = wide[idx];
            }
        }

        BigFloat<N>{
            sign: result_sign,
            exp: exp,
            data: out,
        }
    }

    fn sub_abs(self: BigFloat<N>, rhs: BigFloat<N>, result_sign: bool) {
        let wide: [u32; N + 1] = [0u32; N + 1];
        let out: [u32; N] = [0u32; N];

        for idx in 0..(N + 1) {
            wide[idx] = 0u32;
        }
        for idx in 0..N {
            out[idx] = 0u32;
        }

        let exp = if self.exp >= rhs.exp { self.exp } else { rhs.exp };

        for idx in 0..N {
            let target = (idx as i32) + self.exp - exp;
            if target >= 0i32 && target < ((N + 1) as i32) {
                wide[target as u32] = self.data[idx];
            }
        }

        for idx in 0..N {
            let target = (idx as i32) + rhs.exp - exp;
            if target >= 0i32 && target < ((N + 1) as i32) {
                let pos = target as u32;
                let subbed = bf_sub_borrow(wide[pos], rhs.data[idx], 0u32);
                wide[pos] = subbed[0];
                let borrow = subbed[1];
                let k = pos + 1;
                while borrow != 0u32 && k < (N + 1) {
                    let next = bf_sub_borrow(wide[k], 0u32, borrow);
                    wide[k] = next[0];
                    borrow = next[1];
                    k += 1;
                }
            }
        }

        for idx in 0..N {
            out[idx] = wide[idx];
        }

        let has_limb = false;
        for idx in 0..N {
            if out[idx] != 0u32 {
                has_limb = true;
            }
        }

        if has_limb {
            BigFloat<N>{
                sign: result_sign,
                exp: exp,
                data: out,
            }
        } else {
            BigFloat<N>{
                sign: false,
                exp: 0i32,
                data: out,
            }
        }
    }

    pub fn add(self: BigFloat<N>, rhs: BigFloat<N>) {
        if self.is_zero() {
            rhs
        } else if rhs.is_zero() {
            self
        } else if self.sign == rhs.sign {
            self.add_abs(rhs, self.sign)
        } else {
            let cmp = self.abs_cmp(rhs);
            if cmp > 0i32 {
                self.sub_abs(rhs, self.sign)
            } else if cmp < 0i32 {
                rhs.sub_abs(self, rhs.sign)
            } else {
                let out: [u32; N] = [0u32; N];
                for idx in 0..N {
                    out[idx] = 0u32;
                }
                BigFloat<N>{
                    sign: false,
                    exp: 0i32,
                    data: out,
                }
            }
        }
    }

    pub fn sub(self: BigFloat<N>, rhs: BigFloat<N>) {
        if rhs.is_zero() {
            self
        } else if self.is_zero() {
            BigFloat<N>{
                sign: if rhs.sign { false } else { true },
                exp: rhs.exp,
                data: rhs.data,
            }
        } else if self.sign != rhs.sign {
            self.add_abs(rhs, self.sign)
        } else {
            let cmp = self.abs_cmp(rhs);
            if cmp > 0i32 {
                self.sub_abs(rhs, self.sign)
            } else if cmp < 0i32 {
                rhs.sub_abs(self, if rhs.sign { false } else { true })
            } else {
                let out: [u32; N] = [0u32; N];
                for idx in 0..N {
                    out[idx] = 0u32;
                }
                BigFloat<N>{
                    sign: false,
                    exp: 0i32,
                    data: out,
                }
            }
        }
    }

    fn mul_schoolbook(self: BigFloat<N>, rhs: BigFloat<N>) {
        let low: [u32; N] = self.data;
        let high: [u32; N] = self.data;
        let out: [u32; N] = self.data;

        for idx in 0..N {
            low[idx] = 0u32;
            high[idx] = 0u32;
            out[idx] = 0u32;
        }

        for i in 0..N {
            for j in 0..N {
                let pos = i + j;
                let wide = bf_mul_u32_wide(self.data[i], rhs.data[j]);
                let carry_from_lo = 0u32;

                if pos < N {
                    let lo = bf_add_carry(low[pos], wide[0]);
                    low[pos] = lo[0];
                    carry_from_lo = lo[1];
                } else {
                    let high_pos = pos - N;
                    let lo = bf_add_carry(high[high_pos], wide[0]);
                    high[high_pos] = lo[0];
                    carry_from_lo = lo[1];
                }

                let carry_pos = pos + 1;
                let carry = wide[1];
                if carry_pos < N {
                    let hi = bf_add_carry(low[carry_pos], carry);
                    low[carry_pos] = hi[0];

                    let k = carry_pos + 1;
                    carry = hi[1];
                    while carry != 0u32 && k < N {
                        let next = bf_add_carry(low[k], carry);
                        low[k] = next[0];
                        carry = next[1];
                        k += 1;
                    }

                    k = 0;
                    while carry != 0u32 && k < N {
                        let next = bf_add_carry(high[k], carry);
                        high[k] = next[0];
                        carry = next[1];
                        k += 1;
                    }
                } else {
                    let k = carry_pos - N;
                    while carry != 0u32 && k < N {
                        let next = bf_add_carry(high[k], carry);
                        high[k] = next[0];
                        carry = next[1];
                        k += 1;
                    }
                }

                carry = carry_from_lo;
                if carry_pos < N {
                    let hi = bf_add_carry(low[carry_pos], carry);
                    low[carry_pos] = hi[0];

                    let k = carry_pos + 1;
                    carry = hi[1];
                    while carry != 0u32 && k < N {
                        let next = bf_add_carry(low[k], carry);
                        low[k] = next[0];
                        carry = next[1];
                        k += 1;
                    }

                    k = 0;
                    while carry != 0u32 && k < N {
                        let next = bf_add_carry(high[k], carry);
                        high[k] = next[0];
                        carry = next[1];
                        k += 1;
                    }
                } else {
                    let k = carry_pos - N;
                    while carry != 0u32 && k < N {
                        let next = bf_add_carry(high[k], carry);
                        high[k] = next[0];
                        carry = next[1];
                        k += 1;
                    }
                }
            }
        }

        let exp = self.exp + rhs.exp;
        if high[N - 1] != 0u32 {
            for idx in 0..N {
                out[idx] = high[idx];
                exp += 1;
            }
        } else {
            out[0] = low[N - 1];
            for idx in 1..N {
                out[idx] = high[idx - 1];
                exp += 1;
            }
        }

        BigFloat<N>{
            sign: self.sign != rhs.sign,
            exp: exp,
            data: out,
        }
    }

    fn mul_ss(self: BigFloat<N>, rhs: BigFloat<N>) {
        let modu1 = 998244353u64;
        let modu2 = 1004535809u64;
        let root = 3u64;

        let a1 = BfScratch<N * 4>::zero();
        let b1 = BfScratch<N * 4>::zero();
        let a2 = BfScratch<N * 4>::zero();
        let b2 = BfScratch<N * 4>::zero();

        for idx in 0..N {
            let lo_a = (self.data[idx] & 65535u32) as u64;
            let hi_a = (self.data[idx] >> 16u32) as u64;
            let lo_b = (rhs.data[idx] & 65535u32) as u64;
            let hi_b = (rhs.data[idx] >> 16u32) as u64;
            let pos = idx * 2;
            a1.data[pos] = lo_a % modu1;
            a1.data[pos + 1] = hi_a % modu1;
            b1.data[pos] = lo_b % modu1;
            b1.data[pos + 1] = hi_b % modu1;
            a2.data[pos] = lo_a % modu2;
            a2.data[pos + 1] = hi_a % modu2;
            b2.data[pos] = lo_b % modu2;
            b2.data[pos + 1] = hi_b % modu2;
        }

        let c1 = a1.ntt(false, modu1, root).pointwise(b1.ntt(false, modu1, root), modu1).ntt(true, modu1, root);
        let c2 = a2.ntt(false, modu2, root).pointwise(b2.ntt(false, modu2, root), modu2).ntt(true, modu2, root);

        let wide: [u32; N * 2] = [0u32; N * 2];
        let out: [u32; N] = [0u32; N];
        for idx in 0..(N * 2) {
            wide[idx] = 0u32;
        }
        for idx in 0..N {
            out[idx] = 0u32;
        }

        let inv_modu1 = bf_mod_pow(modu1 % modu2, modu2 - 2u64, modu2);
        let carry = 0u64;
        for coeff_idx in 0..(N * 4) {
            let r1 = c1.data[coeff_idx];
            let r2 = c2.data[coeff_idx];
            let r1_mod_2 = r1 % modu2;
            let diff = if r2 >= r1_mod_2 { r2 - r1_mod_2 } else { r2 + modu2 - r1_mod_2 };
            let t = (diff * inv_modu1) % modu2;
            let coeff = r1 + modu1 * t + carry;
            let digit = coeff & 65535u64;
            carry = coeff >> 16u64;

            let limb_idx = coeff_idx >> 1;
            if limb_idx < N * 2 {
                if (coeff_idx & 1) == 0 {
                    wide[limb_idx] = digit as u32;
                } else {
                    wide[limb_idx] = wide[limb_idx] | ((digit as u32) << 16u32);
                }
            }
        }

        let exp = self.exp + rhs.exp;
        if wide[N * 2 - 1] != 0u32 {
            for idx in 0..N {
                out[idx] = wide[idx + N];
                exp += 1;
            }
        } else {
            out[0] = wide[N - 1];
            for idx in 1..N {
                out[idx] = wide[idx + N - 1];
                exp += 1;
            }
        }

        BigFloat<N>{
            sign: self.sign != rhs.sign,
            exp: exp,
            data: out,
        }
    }

    pub fn mul(self: BigFloat<N>, rhs: BigFloat<N>) {
        if self.is_zero() || rhs.is_zero() {
            let out: [u32; N] = [0u32; N];
            for idx in 0..N {
                out[idx] = 0u32;
            }
            BigFloat<N>{
                sign: false,
                exp: 0i32,
                data: out,
            }
        } else if N >= 32 && (N & (N - 1)) == 0 {
            self.mul_ss(rhs)
        } else {
            self.mul_schoolbook(rhs)
        }
    }
}
