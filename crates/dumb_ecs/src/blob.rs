use std::alloc::{self, Layout};
use std::ptr::NonNull;

/// A type-erased, densely packed vector of components with a runtime layout.
pub struct BlobVec {
    item: Layout,
    drop: unsafe fn(*mut u8),
    ptr: NonNull<u8>,
    len: usize,
    cap: usize,
}

unsafe impl Send for BlobVec {}
unsafe impl Sync for BlobVec {}

impl BlobVec {
    pub fn new(item: Layout, drop: unsafe fn(*mut u8)) -> Self {
        let ptr = dangling(item.align());
        let cap = if item.size() == 0 { usize::MAX } else { 0 };
        BlobVec { item, drop, ptr, len: 0, cap }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn get_ptr(&self, index: usize) -> *mut u8 {
        debug_assert!(index < self.len);
        unsafe { self.ptr.as_ptr().add(index * self.item.size()) }
    }

    fn grow(&mut self, min: usize) {
        let new_cap = (self.cap * 2).max(min).max(8);
        let new_layout = array_layout(self.item, new_cap);
        let new_ptr = unsafe {
            if self.cap == 0 {
                alloc::alloc(new_layout)
            } else {
                alloc::realloc(self.ptr.as_ptr(), array_layout(self.item, self.cap), new_layout.size())
            }
        };
        self.ptr = NonNull::new(new_ptr).unwrap_or_else(|| alloc::handle_alloc_error(new_layout));
        self.cap = new_cap;
    }

    /// Push a new item, letting `write` initialize the slot.
    ///
    /// # Safety
    /// `write` must fully initialize a value of the item type at the given pointer.
    pub unsafe fn push_with(&mut self, write: impl FnOnce(*mut u8)) {
        if self.len == self.cap {
            self.grow(self.len + 1);
        }
        let slot = self.ptr.as_ptr().add(self.len * self.item.size());
        write(slot);
        self.len += 1;
    }

    /// Drop item `index` and move the last item into its place.
    pub fn swap_remove_drop(&mut self, index: usize) {
        assert!(index < self.len);
        let size = self.item.size();
        unsafe {
            let base = self.ptr.as_ptr();
            let hole = base.add(index * size);
            (self.drop)(hole);
            let last = self.len - 1;
            if index != last {
                std::ptr::copy_nonoverlapping(base.add(last * size), hole, size);
            }
        }
        self.len -= 1;
    }

    pub fn clear(&mut self) {
        let size = self.item.size();
        let len = self.len;
        // Set len first so a panicking drop cannot cause a double drop.
        self.len = 0;
        for i in 0..len {
            unsafe { (self.drop)(self.ptr.as_ptr().add(i * size)) };
        }
    }
}

impl Drop for BlobVec {
    fn drop(&mut self) {
        self.clear();
        if self.item.size() != 0 && self.cap != 0 {
            unsafe { alloc::dealloc(self.ptr.as_ptr(), array_layout(self.item, self.cap)) };
        }
    }
}

fn array_layout(item: Layout, n: usize) -> Layout {
    Layout::from_size_align(item.size() * n, item.align()).expect("component array too large")
}

fn dangling(align: usize) -> NonNull<u8> {
    // An aligned non-null address, never dereferenced for size-0 items.
    NonNull::new(align as *mut u8).unwrap()
}
