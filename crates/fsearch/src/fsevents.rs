use std::ffi::{CStr, c_void};
use std::sync::mpsc::Sender;

pub const MUST_SCAN_SUBDIRS: u32 = 0x1;
pub const USER_DROPPED: u32 = 0x2;
pub const KERNEL_DROPPED: u32 = 0x4;
pub const HISTORY_DONE: u32 = 0x10;
const CREATE_FLAG_NO_DEFER: u32 = 0x2;

pub struct Event {
    pub path: Vec<u8>,
    pub flags: u32,
    pub id: u64,

    pub stream: u32,
}

struct Info {
    tx: Sender<Vec<Event>>,
    stream: u32,
}

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type Callback = extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

struct Api {
    create: unsafe extern "C" fn(*const c_void, Callback, *const Context, *const c_void, u64, f64, u32) -> *mut c_void,
    set_queue: unsafe extern "C" fn(*mut c_void, *mut c_void),
    start: unsafe extern "C" fn(*mut c_void) -> u8,
    stop: unsafe extern "C" fn(*mut c_void),
    invalidate: unsafe extern "C" fn(*mut c_void),
    release: unsafe extern "C" fn(*mut c_void),
    current_id: unsafe extern "C" fn() -> u64,
    cf_release: unsafe extern "C" fn(*const c_void),
    string: unsafe extern "C" fn(*const c_void, *const i8, u32) -> *const c_void,
    array: unsafe extern "C" fn(*const c_void, *const *const c_void, isize, *const c_void) -> *const c_void,
    array_callbacks: *const c_void,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> &'static Api {
    static API: std::sync::OnceLock<Api> = std::sync::OnceLock::new();
    API.get_or_init(|| unsafe {
        let open = |path: &CStr| {
            let h = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY);
            assert!(!h.is_null(), "cannot load {path:?}");
            h
        };
        let (cs, cf) = (
            open(c"/System/Library/Frameworks/CoreServices.framework/CoreServices"),
            open(c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"),
        );
        Api {
            create: sym(cs, c"FSEventStreamCreate"),
            set_queue: sym(cs, c"FSEventStreamSetDispatchQueue"),
            start: sym(cs, c"FSEventStreamStart"),
            stop: sym(cs, c"FSEventStreamStop"),
            invalidate: sym(cs, c"FSEventStreamInvalidate"),
            release: sym(cs, c"FSEventStreamRelease"),
            current_id: sym(cs, c"FSEventsGetCurrentEventId"),
            cf_release: sym(cf, c"CFRelease"),
            string: sym(cf, c"CFStringCreateWithCString"),
            array: sym(cf, c"CFArrayCreate"),
            array_callbacks: sym(cf, c"kCFTypeArrayCallBacks"),
        }
    })
}

unsafe fn sym<T>(h: *mut c_void, name: &CStr) -> T {
    let p = unsafe { libc::dlsym(h, name.as_ptr()) };
    assert!(!p.is_null(), "missing {name:?}");
    unsafe { std::mem::transmute_copy(&p) }
}

pub fn current_id() -> u64 {
    unsafe { (api().current_id)() }
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
    fn dispatch_queue_attr_make_with_qos_class(attr: *const c_void, qos: u32, relative_priority: i32) -> *const c_void;
}

extern "C" fn on_events(_s: *mut c_void, info: *mut c_void, n: usize, paths: *mut c_void, flags: *const u32, ids: *const u64) {
    let info = unsafe { &*(info as *const Info) };
    let paths = paths as *const *const i8;
    let batch = (0..n)
        .map(|i| unsafe {
            Event { path: CStr::from_ptr(*paths.add(i)).to_bytes().to_vec(), flags: *flags.add(i), id: *ids.add(i), stream: info.stream }
        })
        .collect();
    let _ = info.tx.send(batch);
}

extern "C" fn release_info(info: *const c_void) {
    if !info.is_null() {
        drop(unsafe { Box::from_raw(info as *mut Info) });
    }
}

pub struct Stream(*mut c_void);

unsafe impl Send for Stream {}
unsafe impl Sync for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let a = api();
            (a.stop)(self.0);
            (a.invalidate)(self.0);
            (a.release)(self.0);
        }
    }
}

pub fn watch(since: u64, latency: f64, tx: Sender<Vec<Event>>, stream: u32) -> Stream {
    let a = api();
    unsafe {
        let root = (a.string)(std::ptr::null(), c"/".as_ptr(), 0x0800_0100);
        let arr = (a.array)(std::ptr::null(), &root, 1, a.array_callbacks);
        let ctx = Context {
            version: 0,
            info: Box::into_raw(Box::new(Info { tx, stream })) as *mut c_void,
            retain: std::ptr::null(),
            release: release_info as *const c_void,
            copy_description: std::ptr::null(),
        };

        let s = (a.create)(std::ptr::null(), on_events, &ctx, arr, since, latency, CREATE_FLAG_NO_DEFER);
        (a.cf_release)(arr);
        (a.cf_release)(root);

        let qos = libc::qos_class_t::QOS_CLASS_USER_INITIATED as u32;
        let q = dispatch_queue_create(c"fsearch.fsevents".as_ptr(), dispatch_queue_attr_make_with_qos_class(std::ptr::null(), qos, 0));
        (a.set_queue)(s, q);
        (a.start)(s);
        Stream(s)
    }
}
