use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    sync::{LazyLock, Mutex, OnceLock},
    time::{Duration, Instant},
};

use openxr_sys as xr;
use openxr_sys::Handle as _;
use wayvr_openxr_layer_common::{BlockMode, ControlReader, Hand};

const LAYER_NAME: &[u8] = b"XR_APILAYER_WAYVR_input_blocker\0";

struct InstanceDispatch {
    enabled: bool,
    instance: xr::Instance,
    next_get_instance_proc_addr: xr::pfn::GetInstanceProcAddr,
    get_action_state_boolean: Option<xr::pfn::GetActionStateBoolean>,
    get_action_state_float: Option<xr::pfn::GetActionStateFloat>,
    get_action_state_vector2f: Option<xr::pfn::GetActionStateVector2f>,
    suggest_interaction_profile_bindings: Option<xr::pfn::SuggestInteractionProfileBindings>,
    path_to_string: Option<xr::pfn::PathToString>,
    string_to_path: Option<xr::pfn::StringToPath>,
    get_current_interaction_profile: Option<xr::pfn::GetCurrentInteractionProfile>,
    /// `/user/hand/{left,right}` atoms, indexed by [`Hand::index`].
    hand_paths: Option<[xr::Path; 2]>,
    /// Interaction profile the runtime currently has bound to each hand, or 0
    /// while unknown. Only bindings of the active profile can actually fire, so
    /// this is what makes a per-component decision meaningful: the same action
    /// is a grip on the profile in use and a trigger on one that is not.
    active_profiles: [u64; 2],
    /// When [`active_profiles`](Self::active_profiles) was last re-read. The
    /// profile changes when controllers wake, sleep or are swapped, and we do
    /// not intercept the event that announces it, so it is polled instead.
    profiles_refreshed: Option<Instant>,
    /// Every suggested binding of an action, keyed by its interned `Action`
    /// atom (stable for the life of the instance). Classification must be kept
    /// per binding rather than collapsed per action: apps (via xrizer) reuse one
    /// action across every controller profile and both hands, so the same action
    /// is a thumbstick click on Oculus yet a trackpad/button on Vive/Index, and
    /// its left-hand copy must be able to block while the right-hand one passes.
    bindings: HashMap<u64, Vec<BindingInfo>>,
    /// Resolved hand of `ActionStateGetInfo::subaction_path` atoms. Queried
    /// every frame for every action, so the string round-trip is cached.
    subaction_hands: HashMap<u64, Option<Hand>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingKind {
    /// Never withheld, whatever the block mode: passive components an app polls
    /// for presence rather than intent.
    NeverBlock,
    /// The component wayvr itself consumes when you point at an overlay.
    Trigger,
    Other,
}

#[derive(Clone, Copy)]
struct BindingInfo {
    /// Interned interaction-profile `Path` atom.
    profile: u64,
    /// `None` for bindings that are not under `/user/hand/{left,right}` (head,
    /// gamepad, tracker...), which no hand can claim.
    hand: Option<Hand>,
    kind: BindingKind,
}

fn binding_kind(path: &str) -> BindingKind {
    if path.ends_with("/touch")
        || path.ends_with("/thumbstick/click")
        || path.ends_with("/joystick/click")
    {
        BindingKind::NeverBlock
    } else if path.contains("/input/trigger") || path.contains("/input/select") {
        // `select` is the simple-controller profile's stand-in for the trigger.
        BindingKind::Trigger
    } else {
        BindingKind::Other
    }
}

fn binding_hand(path: &str) -> Option<Hand> {
    if path.starts_with("/user/hand/left") {
        Some(Hand::Left)
    } else if path.starts_with("/user/hand/right") {
        Some(Hand::Right)
    } else {
        None
    }
}

/// Resolve a `Path` atom to its string form (two-call capacity probe).
fn path_to_string(
    func: xr::pfn::PathToString,
    instance: xr::Instance,
    path: xr::Path,
) -> Option<String> {
    let mut count: u32 = 0;
    let result = unsafe { func(instance, path, 0, &mut count, std::ptr::null_mut()) };
    if result != xr::Result::SUCCESS || count == 0 {
        return None;
    }
    let mut buf = vec![0u8; count as usize];
    let result = unsafe { func(instance, path, count, &mut count, buf.as_mut_ptr().cast()) };
    if result != xr::Result::SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).ok().map(str::to_owned)
}

static CONTROL: LazyLock<ControlReader> = LazyLock::new(ControlReader::new);
static DISPATCH: Mutex<Option<InstanceDispatch>> = Mutex::new(None);

fn fallback_next_get_instance_proc_addr() -> &'static OnceLock<xr::pfn::GetInstanceProcAddr> {
    static NEXT_GIPA: OnceLock<xr::pfn::GetInstanceProcAddr> = OnceLock::new();
    &NEXT_GIPA
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn xrNegotiateLoaderApiLayerInterface(
    loader_info: *const xr::NegotiateLoaderInfo,
    layer_name: *const c_char,
    api_layer_request: *mut xr::NegotiateApiLayerRequest,
) -> xr::Result {
    if loader_info.is_null() || api_layer_request.is_null() || layer_name.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }

    if unsafe { CStr::from_ptr(layer_name) }.to_bytes_with_nul() != LAYER_NAME {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    let loader_info = unsafe { &*loader_info };
    let api_layer_request = unsafe { &mut *api_layer_request };

    if loader_info.min_interface_version > xr::CURRENT_LOADER_API_LAYER_VERSION as u32
        || loader_info.max_interface_version < xr::CURRENT_LOADER_API_LAYER_VERSION as u32
        || loader_info.min_api_version > xr::CURRENT_API_VERSION
        || loader_info.max_api_version < xr::CURRENT_API_VERSION
    {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    api_layer_request.layer_interface_version = xr::CURRENT_LOADER_API_LAYER_VERSION as u32;
    api_layer_request.layer_api_version = xr::CURRENT_API_VERSION;
    api_layer_request.get_instance_proc_addr = Some(xrGetInstanceProcAddr);
    api_layer_request.create_api_layer_instance = Some(xrCreateApiLayerInstance);
    xr::Result::SUCCESS
}

#[allow(non_snake_case)]
unsafe extern "system" fn xrGetInstanceProcAddr(
    instance: xr::Instance,
    name: *const c_char,
    function: *mut Option<xr::pfn::VoidFunction>,
) -> xr::Result {
    if name.is_null() || function.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }

    let name = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();

    if name == b"xrGetInstanceProcAddr\0" {
        unsafe {
            *function = Some(void_fn::<xr::pfn::GetInstanceProcAddr>(
                xrGetInstanceProcAddr,
            ));
        }
        return xr::Result::SUCCESS;
    }

    let intercept: Option<xr::pfn::VoidFunction> = match name {
        b"xrGetActionStateBoolean\0" => Some(void_fn::<xr::pfn::GetActionStateBoolean>(
            xrGetActionStateBoolean,
        )),
        b"xrGetActionStateFloat\0" => Some(void_fn::<xr::pfn::GetActionStateFloat>(
            xrGetActionStateFloat,
        )),
        b"xrGetActionStateVector2f\0" => Some(void_fn::<xr::pfn::GetActionStateVector2f>(
            xrGetActionStateVector2f,
        )),
        b"xrSuggestInteractionProfileBindings\0" => {
            Some(void_fn::<xr::pfn::SuggestInteractionProfileBindings>(
                xrSuggestInteractionProfileBindings,
            ))
        }
        _ => None,
    };

    let next_gipa = {
        let Ok(guard) = DISPATCH.lock() else {
            return xr::Result::ERROR_RUNTIME_FAILURE;
        };
        guard
            .as_ref()
            .map(|dispatch| dispatch.next_get_instance_proc_addr)
            .or_else(|| fallback_next_get_instance_proc_addr().get().copied())
    };

    let Some(next_gipa) = next_gipa else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };

    let result = unsafe { next_gipa(instance, name.as_ptr().cast(), function) };
    if result != xr::Result::SUCCESS {
        return result;
    }

    if intercept.is_some() {
        if let Ok(mut guard) = DISPATCH.lock()
            && let Some(dispatch) = guard.as_mut()
            && dispatch.instance == instance
        {
            match name {
                b"xrGetActionStateBoolean\0" => {
                    dispatch.get_action_state_boolean =
                        unsafe { Some(std::mem::transmute_copy(&*function)) };
                }
                b"xrGetActionStateFloat\0" => {
                    dispatch.get_action_state_float =
                        unsafe { Some(std::mem::transmute_copy(&*function)) };
                }
                b"xrGetActionStateVector2f\0" => {
                    dispatch.get_action_state_vector2f =
                        unsafe { Some(std::mem::transmute_copy(&*function)) };
                }
                b"xrSuggestInteractionProfileBindings\0" => {
                    dispatch.suggest_interaction_profile_bindings =
                        unsafe { Some(std::mem::transmute_copy(&*function)) };
                }
                _ => {}
            }
        }

        unsafe {
            *function = intercept;
        }
    }

    xr::Result::SUCCESS
}

#[allow(non_snake_case)]
unsafe extern "system" fn xrCreateApiLayerInstance(
    info: *const xr::InstanceCreateInfo,
    layer_info: *const xr::ApiLayerCreateInfo,
    instance: *mut xr::Instance,
) -> xr::Result {
    if info.is_null() || layer_info.is_null() || instance.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }

    let info_ref = unsafe { &*info };
    let layer_info_ref = unsafe { &*layer_info };
    let next_info = layer_info_ref.next_info;
    if next_info.is_null() {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    let next_info_ref = unsafe { &*next_info };
    if app_name(&next_info_ref.layer_name) != "XR_APILAYER_WAYVR_input_blocker" {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }
    let Some(next_create) = next_info_ref.next_create_api_layer_instance else {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next_gipa) = next_info_ref.next_get_instance_proc_addr else {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    };

    // Safeguard against layer being loaded twice
    let next_is_duplicate_self = next_create as usize
        == xrCreateApiLayerInstance as xr::pfn::CreateApiLayerInstance as usize;
    if next_is_duplicate_self {
        eprintln!(
            "[wayvr-openxr-layer] FATAL: this layer is present more than once in the \
             OpenXR instance chain. Aborting instead of recursing into a stack \
             overflow. Ensure exactly one wayvr-input-blocker manifest is on \
             XDG_DATA_DIRS.",
        );
        std::process::abort();
    }

    let _ = fallback_next_get_instance_proc_addr().set(next_gipa);

    let mut forwarded_layer_info = *layer_info_ref;
    forwarded_layer_info.next_info = next_info_ref.next;

    let result = unsafe { next_create(info, &forwarded_layer_info, instance) };
    if result != xr::Result::SUCCESS {
        return result;
    }

    let created_instance = unsafe { *instance };
    let app = app_name(&info_ref.application_info.application_name);
    let enabled = app != "wayvr";
    let snap = CONTROL.debug_snapshot();
    eprintln!(
        "[wayvr-openxr-layer] loaded into OpenXR instance (app={app:?}, input-blocking {}); \
         control: path={:?} mapped={} live={} heartbeat_age_ms={} version={} raw_flags={}",
        if enabled { "armed" } else { "disabled (self)" },
        wayvr_openxr_layer_common::control_path(),
        snap.mapped,
        snap.live,
        snap.heartbeat_age_ms,
        snap.version,
        snap.raw_flags,
    );
    let dispatch = InstanceDispatch {
        enabled,
        instance: created_instance,
        next_get_instance_proc_addr: next_gipa,
        get_action_state_boolean: None,
        get_action_state_float: None,
        get_action_state_vector2f: None,
        suggest_interaction_profile_bindings: None,
        path_to_string: None,
        string_to_path: None,
        get_current_interaction_profile: None,
        hand_paths: None,
        active_profiles: [0; 2],
        profiles_refreshed: None,
        bindings: HashMap::new(),
        subaction_hands: HashMap::new(),
    };

    if let Ok(mut guard) = DISPATCH.lock() {
        *guard = Some(dispatch);
    }

    xr::Result::SUCCESS
}

// The three xrGetActionState* entry points are identical except for the next
// pointer they forward to, their state struct, and the resting value written to
// `current_state` when blocked. Generate them rather than triplicating the
// forward-then-zero boilerplate.
macro_rules! get_action_state_shim {
    ($name:ident, $field:ident, $state:ty, $blocked:expr) => {
        #[allow(non_snake_case)]
        unsafe extern "system" fn $name(
            session: xr::Session,
            get_info: *const xr::ActionStateGetInfo,
            state: *mut $state,
        ) -> xr::Result {
            let result = with_dispatch(|dispatch| match dispatch.$field {
                Some(func) => unsafe { func(session, get_info, state) },
                None => xr::Result::ERROR_FUNCTION_UNSUPPORTED,
            });
            if result == xr::Result::SUCCESS && should_block_action(session, get_info) {
                unsafe {
                    (*state).current_state = $blocked;
                    (*state).changed_since_last_sync = xr::FALSE;
                    (*state).last_change_time = xr::Time::from_nanos(0);
                    (*state).is_active = xr::FALSE;
                }
            }
            result
        }
    };
}

get_action_state_shim!(
    xrGetActionStateBoolean,
    get_action_state_boolean,
    xr::ActionStateBoolean,
    xr::FALSE
);
get_action_state_shim!(
    xrGetActionStateFloat,
    get_action_state_float,
    xr::ActionStateFloat,
    0.0
);
get_action_state_shim!(
    xrGetActionStateVector2f,
    get_action_state_vector2f,
    xr::ActionStateVector2f,
    xr::Vector2f { x: 0.0, y: 0.0 }
);

fn with_dispatch(f: impl FnOnce(&InstanceDispatch) -> xr::Result) -> xr::Result {
    let Ok(guard) = DISPATCH.lock() else {
        return xr::Result::ERROR_RUNTIME_FAILURE;
    };
    let Some(dispatch) = guard.as_ref() else {
        return xr::Result::ERROR_HANDLE_INVALID;
    };
    f(dispatch)
}

#[allow(non_snake_case)]
unsafe extern "system" fn xrSuggestInteractionProfileBindings(
    instance: xr::Instance,
    suggested_bindings: *const xr::InteractionProfileSuggestedBinding,
) -> xr::Result {
    // Record which actions map to never-block components, then forward.
    let next = {
        let Ok(mut guard) = DISPATCH.lock() else {
            return xr::Result::ERROR_RUNTIME_FAILURE;
        };
        let Some(dispatch) = guard.as_mut() else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if dispatch.instance == instance
            && !suggested_bindings.is_null()
            && let Some(p2s) = ensure_path_to_string(dispatch, instance)
        {
            let info = unsafe { &*suggested_bindings };
            if !info.suggested_bindings.is_null() {
                let bindings = unsafe {
                    std::slice::from_raw_parts(
                        info.suggested_bindings,
                        info.count_suggested_bindings as usize,
                    )
                };
                let profile = info.interaction_profile.into_raw();
                for binding in bindings {
                    let action = binding.action.into_raw();
                    let path = path_to_string(p2s, instance, binding.binding);
                    let kind = path.as_deref().map_or(BindingKind::Other, binding_kind);
                    let hand = path.as_deref().and_then(binding_hand);
                    if dispatch.enabled {
                        eprintln!(
                            "[wayvr-openxr-layer] binding: {} -> {} ({})",
                            path.as_deref().unwrap_or("<unknown>"),
                            match kind {
                                BindingKind::NeverBlock => "pass-through",
                                BindingKind::Trigger => "blockable (trigger)",
                                BindingKind::Other => "blockable",
                            },
                            match hand {
                                Some(Hand::Left) => "left",
                                Some(Hand::Right) => "right",
                                None => "no hand",
                            },
                        );
                    }
                    dispatch
                        .bindings
                        .entry(action)
                        .or_default()
                        .push(BindingInfo {
                            profile,
                            hand,
                            kind,
                        });
                }
            }
        }
        dispatch.suggest_interaction_profile_bindings
    };

    let Some(next) = next else {
        return xr::Result::ERROR_FUNCTION_UNSUPPORTED;
    };
    unsafe { next(instance, suggested_bindings) }
}

/// How long an `active_profiles` reading is reused before being polled again.
/// Long enough that the runtime call is negligible next to the per-frame action
/// queries, short enough that picking up a controller re-arms blocking promptly.
const PROFILE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Look up an OpenXR entry point the app never asked us for. `F` must be a real
/// `fn` pointer type, as in [`void_fn`].
fn resolve<F: Copy>(dispatch: &InstanceDispatch, instance: xr::Instance, name: &CStr) -> Option<F> {
    const {
        assert!(std::mem::size_of::<F>() == std::mem::size_of::<xr::pfn::VoidFunction>());
    }
    let mut func: Option<xr::pfn::VoidFunction> = None;
    let result =
        unsafe { (dispatch.next_get_instance_proc_addr)(instance, name.as_ptr(), &mut func) };
    if result != xr::Result::SUCCESS {
        return None;
    }
    func.map(|f| unsafe { std::mem::transmute_copy(&f) })
}

/// Resolve `xrPathToString` on demand — not every app requests it itself, and
/// the suggested-bindings call is not guaranteed to have run first.
fn ensure_path_to_string(
    dispatch: &mut InstanceDispatch,
    instance: xr::Instance,
) -> Option<xr::pfn::PathToString> {
    if dispatch.path_to_string.is_none() {
        dispatch.path_to_string = resolve(dispatch, instance, c"xrPathToString");
    }
    dispatch.path_to_string
}

/// Re-read which interaction profile each hand is bound to, at most every
/// [`PROFILE_POLL_INTERVAL`]. Leaves a hand at 0 (unknown) if the runtime has
/// not bound it yet, which it does not do until the session is running.
fn refresh_active_profiles(dispatch: &mut InstanceDispatch, session: xr::Session) {
    if dispatch
        .profiles_refreshed
        .is_some_and(|at| at.elapsed() < PROFILE_POLL_INTERVAL)
    {
        return;
    }
    dispatch.profiles_refreshed = Some(Instant::now());

    let instance = dispatch.instance;
    if dispatch.hand_paths.is_none() {
        if dispatch.string_to_path.is_none() {
            dispatch.string_to_path = resolve(dispatch, instance, c"xrStringToPath");
        }
        let Some(s2p) = dispatch.string_to_path else {
            return;
        };
        let mut paths = [xr::Path::NULL; 2];
        for (path, name) in paths
            .iter_mut()
            .zip([c"/user/hand/left", c"/user/hand/right"])
        {
            if unsafe { s2p(instance, name.as_ptr(), path) } != xr::Result::SUCCESS {
                return;
            }
        }
        dispatch.hand_paths = Some(paths);
    }

    if dispatch.get_current_interaction_profile.is_none() {
        dispatch.get_current_interaction_profile =
            resolve(dispatch, instance, c"xrGetCurrentInteractionProfile");
    }
    let (Some(get_profile), Some(hand_paths)) = (
        dispatch.get_current_interaction_profile,
        dispatch.hand_paths,
    ) else {
        return;
    };

    let enabled = dispatch.enabled;
    let p2s = ensure_path_to_string(dispatch, instance);
    for (idx, hand_path) in hand_paths.into_iter().enumerate() {
        let mut state = xr::InteractionProfileState::out(std::ptr::null_mut());
        if unsafe { get_profile(session, hand_path, state.as_mut_ptr()) } != xr::Result::SUCCESS {
            continue;
        }
        let profile = unsafe { state.assume_init() }.interaction_profile;
        let raw = profile.into_raw();
        if dispatch.active_profiles[idx] == raw {
            continue;
        }
        dispatch.active_profiles[idx] = raw;
        if enabled {
            let name = p2s
                .and_then(|p2s| path_to_string(p2s, instance, profile))
                .unwrap_or_else(|| "<none>".to_owned());
            eprintln!(
                "[wayvr-openxr-layer] {} hand interaction profile: {name}",
                if idx == 0 { "left" } else { "right" },
            );
        }
    }
}

/// Hand named by an `ActionStateGetInfo::subaction_path`, memoized per atom.
fn subaction_hand(dispatch: &mut InstanceDispatch, path: xr::Path) -> Option<Hand> {
    let raw = path.into_raw();
    if raw == 0 {
        return None;
    }
    if let Some(hand) = dispatch.subaction_hands.get(&raw) {
        return *hand;
    }
    let instance = dispatch.instance;
    let hand = ensure_path_to_string(dispatch, instance)
        .and_then(|p2s| path_to_string(p2s, instance, path))
        .as_deref()
        .and_then(binding_hand);
    dispatch.subaction_hands.insert(raw, hand);
    hand
}

/// Whether the action queried by `get_info` should have its state zeroed.
///
/// Each hand is judged on its own: an action is blocked only if some binding
/// that can actually produce it belongs to a hand whose block mode covers it.
/// An action bound symmetrically to both hands and queried without a subaction
/// path is indistinguishable at this level, so either blocked hand blocks it —
/// that is the one case where the hands are not independent, and it is the app's
/// binding layout, not this layer, that merges them.
fn should_block_action(session: xr::Session, get_info: *const xr::ActionStateGetInfo) -> bool {
    let Ok(mut guard) = DISPATCH.lock() else {
        return false;
    };
    let Some(dispatch) = guard.as_mut() else {
        return false;
    };
    if !dispatch.enabled {
        return false;
    }

    let left = CONTROL.block_mode(Hand::Left);
    let right = CONTROL.block_mode(Hand::Right);
    if left == BlockMode::None && right == BlockMode::None {
        return false;
    }
    let mode_of = |hand: Option<Hand>| match hand {
        Some(Hand::Left) => left,
        Some(Hand::Right) => right,
        // A binding no hand owns is only withheld while both hands are fully
        // blocked, so a single pointing hand can never mute the other's actions.
        None => left.min_mode(right),
    };

    if get_info.is_null() {
        return left == BlockMode::All && right == BlockMode::All;
    }
    let get_info = unsafe { &*get_info };
    let action = get_info.action.into_raw();
    let queried_hand = subaction_hand(dispatch, get_info.subaction_path);
    refresh_active_profiles(dispatch, session);
    let active_profiles = dispatch.active_profiles;

    let Some(bindings) = dispatch.bindings.get(&action) else {
        // Never saw this action suggested; fall back to the coarse behaviour.
        return mode_of(queried_hand) == BlockMode::All;
    };

    // Bindings of a profile that is not in use cannot fire, and must not colour
    // the decision: a game's Grab is a grip on the controller you are holding
    // and, on some other profile it also suggested, the trigger. Judging it by
    // that unused trigger binding would take grab away in trigger-only mode.
    // A hand whose profile is still unknown keeps all of its bindings in scope.
    let profile_in_use = move |b: &BindingInfo| {
        let active = match b.hand {
            Some(hand) => active_profiles[hand.index()],
            // Not a hand's binding, so either hand's profile may carry it.
            None => return active_profiles.iter().any(|&p| p == 0 || p == b.profile),
        };
        active == 0 || active == b.profile
    };

    // A subaction path narrows the query to one hand's bindings; hand-less
    // bindings stay in scope because the runtime may still route them there.
    let relevant = bindings
        .iter()
        .filter(|b| queried_hand.is_none() || b.hand.is_none() || b.hand == queried_hand)
        .filter(|b| profile_in_use(b));

    relevant
        .clone()
        .any(|b| match (b.kind, mode_of(b.hand)) {
            (BindingKind::NeverBlock, _) | (_, BlockMode::None) => false,
            (BindingKind::Trigger, _) => true,
            (BindingKind::Other, mode) => mode == BlockMode::All,
        })
        // ...unless the action reaches a never-block component on a profile
        // where nothing about it is blockable: apps reuse one action across
        // profiles, and the passive component defines its meaning there.
        && !relevant.clone().any(|allow| {
            allow.kind == BindingKind::NeverBlock
                && !relevant.clone().any(|block| {
                    block.profile == allow.profile && block.kind != BindingKind::NeverBlock
                })
        })
}

/// Reinterpret a concrete OpenXR function pointer as the type-erased
/// [`xr::pfn::VoidFunction`] the loader expects.
///
/// The caller must pass `func` already coerced to a real `fn` pointer type `F`
/// (e.g. `xr::pfn::GetActionStateBoolean`), *not* a bare function item. A
/// function item is a zero-sized type, and transmuting from it would read 8
/// bytes out of a 0-byte value and abort. Taking `F` by value forces the
/// coercion at the call site so `F` is always pointer-sized.
fn void_fn<F: Copy>(func: F) -> xr::pfn::VoidFunction {
    const {
        assert!(
            std::mem::size_of::<F>() == std::mem::size_of::<xr::pfn::VoidFunction>(),
            "void_fn expects a real fn pointer, not a zero-sized fn item",
        );
    }
    unsafe { std::mem::transmute_copy(&func) }
}

fn app_name(name: &[c_char]) -> &str {
    let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    let bytes = unsafe { std::slice::from_raw_parts(name.as_ptr().cast::<u8>(), len) };
    std::str::from_utf8(bytes).unwrap_or_default()
}
