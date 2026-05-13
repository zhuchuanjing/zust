use bitvec::prelude::*;
use std::iter::FusedIterator;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub struct FixVec<T> {
    buf: Vec<T>,
    useds: BitVec,
    len: AtomicUsize,
}

impl<T: Default> FixVec<T> {
    pub fn push(&mut self, v: T) -> usize {
        self.len.fetch_add(1, Ordering::Relaxed);
        if let Some(pos) = self.useds.first_zero() {
            self.buf[pos] = v;
            self.useds.set(pos, true);
            pos
        } else {
            self.buf.push(v);
            self.useds.push(true);
            self.buf.len() - 1
        }
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }

    pub fn pop(&mut self) -> Option<T> {
        if let Some(idx) = self.useds.iter().enumerate().rev().find(|(_idx, val)| *val.as_ref()).map(|(idx, _)| idx) { self.remove(idx) } else { None }
    }

    pub fn remove(&mut self, pos: usize) -> Option<T> {
        if self.useds.get(pos).map(|v| *v).unwrap_or(false) {
            self.len.fetch_sub(1, Ordering::Relaxed);
            self.useds.set(pos, false);
            Some(std::mem::take(&mut self.buf[pos]))
        } else {
            None
        }
    }

    pub fn get(&self, pos: usize) -> Option<&T> {
        if self.useds.get(pos).map(|v| *v).unwrap_or(false) { self.buf.get(pos) } else { None }
    }

    pub fn get_mut(&mut self, pos: usize) -> Option<&mut T> {
        if self.useds.get(pos).map(|v| *v).unwrap_or(false) { self.buf.get_mut(pos) } else { None }
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter { vec: self, index: 0 }
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        IterMut { vec: self, index: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.useds.not_any()
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn clear(&mut self) {
        self.len.store(0, Ordering::Relaxed);
        for i in 0..self.buf.len() {
            if self.useds.get(i).map(|v| *v).unwrap_or(false) {
                let _ = std::mem::take(&mut self.buf[i]);
            }
        }
        self.useds.fill(false);
    }

    pub fn contains(&self, index: usize) -> bool {
        self.useds.get(index).map(|v| *v).unwrap_or(false)
    }

    pub fn indices(&self) -> Indices<'_, T> {
        Indices { vec: self, index: 0 }
    }

    pub fn values(&self) -> Values<'_, T> {
        Values { iter: self.iter() }
    }

    pub fn values_mut(&mut self) -> ValuesMut<'_, T> {
        ValuesMut { iter: self.iter_mut() }
    }
}

pub struct Iter<'a, T> {
    vec: &'a FixVec<T>,
    index: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = (usize, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.vec.buf.len() {
            let current_index = self.index;
            self.index += 1;

            if self.vec.useds.get(current_index).map(|v| *v).unwrap_or(false) {
                return Some((current_index, &self.vec.buf[current_index]));
            }
        }
        None
    }
}

impl<'a, T> ExactSizeIterator for Iter<'a, T> {}
impl<'a, T> FusedIterator for Iter<'a, T> {}

pub struct IterMut<'a, T> {
    vec: &'a mut FixVec<T>,
    index: usize,
}

impl<'a, T> Iterator for IterMut<'a, T> {
    type Item = (usize, &'a mut T);
    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.vec.buf.len() {
            let current_index = self.index;
            self.index += 1;
            if self.vec.useds.get(current_index).map(|v| *v).unwrap_or(false) {
                unsafe {
                    let ptr = self.vec.buf.as_mut_ptr().add(current_index);
                    return Some((current_index, &mut *ptr));
                }
            }
        }
        None
    }
}

impl<'a, T> ExactSizeIterator for IterMut<'a, T> {}
impl<'a, T> FusedIterator for IterMut<'a, T> {}

impl<T: Default> IntoIterator for FixVec<T> {
    type Item = (usize, T);
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter { vec: self, index: 0 }
    }
}

pub struct IntoIter<T: Default> {
    vec: FixVec<T>,
    index: usize,
}

impl<T: Default> Iterator for IntoIter<T> {
    type Item = (usize, T);

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.vec.buf.len() {
            let current_index = self.index;
            self.index += 1;
            if self.vec.useds.get(current_index).map(|v| *v).unwrap_or(false) {
                self.vec.useds.set(current_index, false);
                let value = std::mem::take(&mut self.vec.buf[current_index]);
                return Some((current_index, value));
            }
        }
        None
    }
}

impl<T: Default> ExactSizeIterator for IntoIter<T> {}
impl<T: Default> FusedIterator for IntoIter<T> {}

impl<T: Default> Drop for IntoIter<T> {
    fn drop(&mut self) {
        while self.index < self.vec.buf.len() {
            if self.vec.useds.get(self.index).map(|v| *v).unwrap_or(false) {
                self.vec.useds.set(self.index, false);
                let _ = std::mem::take(&mut self.vec.buf[self.index]);
            }
            self.index += 1;
        }
    }
}

impl<'a, T: Default> IntoIterator for &'a FixVec<T> {
    type Item = (usize, &'a T);
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: Default> IntoIterator for &'a mut FixVec<T> {
    type Item = (usize, &'a mut T);
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

pub struct Indices<'a, T> {
    vec: &'a FixVec<T>,
    index: usize,
}

impl<'a, T> Iterator for Indices<'a, T> {
    type Item = usize;
    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.vec.buf.len() {
            let current_index = self.index;
            self.index += 1;

            if self.vec.useds.get(current_index).map(|v| *v).unwrap_or(false) {
                return Some(current_index);
            }
        }
        None
    }
}

impl<'a, T> ExactSizeIterator for Indices<'a, T> {}
impl<'a, T> FusedIterator for Indices<'a, T> {}

pub struct Values<'a, T> {
    iter: Iter<'a, T>,
}

impl<'a, T> Iterator for Values<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a, T> ExactSizeIterator for Values<'a, T> {}
impl<'a, T> FusedIterator for Values<'a, T> {}

pub struct ValuesMut<'a, T> {
    iter: IterMut<'a, T>,
}

impl<'a, T> Iterator for ValuesMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a, T> ExactSizeIterator for ValuesMut<'a, T> {}
impl<'a, T> FusedIterator for ValuesMut<'a, T> {}
