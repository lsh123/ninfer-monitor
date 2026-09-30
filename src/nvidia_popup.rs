// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

//! Disabling the NVIDIA game popup via a per-executable app profile (M6,
//! Windows only).
//!
//! NVIDIA shows a "game popup" the first time the driver sees a new
//! executable. The driver settings (DRS) API registers per-executable app
//! profiles; a magic setting (`0x809D5F60 = 0x10000000`, DWORD) in the profile
//! suppresses the popup. This module adds and removes that profile for
//! [`EXE_NAME`]:
//!
//! - [`add_profile`] (CLI `--add-nvidia-app-profile`, the "Disable NVIDIA
//!   popup" settings link, and the installer's install hook) creates the
//!   profile, registers the executable, and sets the magic value. Re-adding an
//!   existing profile is a no-op overwrite.
//! - [`remove_profile`] (CLI `--delete-nvidia-app-profile` and the installer's
//!   uninstall hook) deletes the profile; a missing profile counts as
//!   success, so it is idempotent.
//!
//! `nvapi64.dll` is loaded at run time only when a profile operation runs
//! (NFR-1/NFR-4: the app must start on machines without the NVIDIA driver), so
//! the executable has no load-time dependency on the driver. The `nvapi` crate
//! contributes only its pure-Rust `Status` enum, used to name errors.

/// The DRS profile name created for this app.
pub const PROFILE_NAME: &str = "NInferMonitorProfile";

/// The executable name registered in the profile. It must match the shipped
/// binary name, or the driver will not recognize the app.
pub const EXE_NAME: &str = "ninfer-monitor.exe";

/// The Settings tab status string shown on success (PRD v0.2 §3.2).
pub const SUCCESS_MESSAGE: &str = "NVIDIA game popup disabled.";

/// Adds the app profile that disables the NVIDIA game popup for [`EXE_NAME`].
///
/// On non-Windows platforms this always fails: the feature is Windows only.
pub fn add_profile() -> Result<(), String> {
    #[cfg(windows)]
    {
        win::add_profile()
    }
    #[cfg(not(windows))]
    {
        Err("NVIDIA profile support is only available on Windows".to_owned())
    }
}

/// Removes the app profile created by [`add_profile`]. Removing a profile that
/// does not exist is a success (idempotent), so the uninstall hook is safe to
/// run on every uninstall.
pub fn remove_profile() -> Result<(), String> {
    #[cfg(windows)]
    {
        win::remove_profile()
    }
    #[cfg(not(windows))]
    {
        Err("NVIDIA profile support is only available on Windows".to_owned())
    }
}

/// Adds the app profile for the Settings tab: it re-launches the app
/// elevated (a UAC prompt) when this process lacks the privilege to write the
/// machine-wide (per-machine) profile, waits for the elevated copy to finish,
/// and reports its result. [`add_profile`] is the in-process operation used by
/// the CLI path, which relies on the installer's own elevation.
pub fn add_profile_interactive() -> Result<(), String> {
    #[cfg(windows)]
    {
        win::run_interactive(true)
    }
    #[cfg(not(windows))]
    {
        Err("NVIDIA profile support is only available on Windows".to_owned())
    }
}

/// Removes the app profile for the Settings tab, re-launching elevated (UAC)
/// when not privileged (see [`add_profile_interactive`]).
pub fn remove_profile_interactive() -> Result<(), String> {
    #[cfg(windows)]
    {
        win::run_interactive(false)
    }
    #[cfg(not(windows))]
    {
        Err("NVIDIA profile support is only available on Windows".to_owned())
    }
}

/// The Windows NVAPI (DRS) implementation. This module is one of the crate's
/// isolated `unsafe` areas, per NFR-7: the NVAPI FFI is isolated here and
/// every `unsafe` block is justified by an adjacent `// SAFETY:` comment.
#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::mem::{size_of, transmute_copy};
    use std::ptr;

    use nvapi::Status;
    use windows::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{s, w};

    /// `NvAPI_UnicodeString` (`nvapi.h`): 2048 `NvU16`.
    pub(super) type NvApiUnicodeString = [u16; 2048];

    /// `NVDRS_PROFILE_V1` (`nvapi.h`): 4116 bytes, alignment 4.
    #[repr(C)]
    pub(super) struct NvdrsProfile {
        version: u32,
        profile_name: NvApiUnicodeString,
        gpu_support: u32,
        is_predefined: u32,
        num_of_apps: u32,
        num_of_settings: u32,
    }

    /// `NVDRS_APPLICATION_V4` (`nvapi.h`): 20492 bytes, alignment 4.
    #[repr(C)]
    pub(super) struct NvdrsApplication {
        version: u32,
        is_predefined: u32,
        app_name: NvApiUnicodeString,
        user_friendly_name: NvApiUnicodeString,
        launcher: NvApiUnicodeString,
        file_in_folder: NvApiUnicodeString,
        /// `isMetro:1 | isCommandLine:1 | reserved:30` bitfield.
        flags: u32,
        command_line: NvApiUnicodeString,
    }

    /// `NVDRS_BINARY_SETTING` (`nvapi.h`).
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct NvdrsBinarySetting {
        value_length: u32,
        value_data: [u8; 4096],
    }

    /// The value union of `NVDRS_SETTING` (`nvapi.h`); only the DWORD variant
    /// is used. The header wraps the struct in `#pragma pack(push, 4)`, which
    /// caps the union alignment at 4; the `u64` variant of the C union is
    /// omitted here so the Rust union has the same alignment (the layout is
    /// pinned by `tests`).
    #[repr(C)]
    #[allow(dead_code)]
    pub(super) union SettingValue {
        dword: u32,
        wstring: NvApiUnicodeString,
        binary: NvdrsBinarySetting,
    }

    /// `NVDRS_SETTING_V1` (`nvapi.h`): 12320 bytes, alignment 4.
    #[repr(C)]
    pub(super) struct NvdrsSetting {
        version: u32,
        setting_name: NvApiUnicodeString,
        setting_id: u32,
        setting_type: u32,
        setting_location: u32,
        is_current_predefined: u32,
        is_predefined_valid: u32,
        predefined: SettingValue,
        current: SettingValue,
    }

    /// `MAKE_NVAPI_VERSION(type, ver)` (`nvapi_lite_common.h`): the struct
    /// size in the low 16 bits, the version in the high 16 bits.
    pub(super) const PROFILE_VERSION: u32 = (size_of::<NvdrsProfile>() as u32) | (1 << 16);
    pub(super) const APPLICATION_VERSION: u32 = (size_of::<NvdrsApplication>() as u32) | (4 << 16);
    pub(super) const SETTING_VERSION: u32 = (size_of::<NvdrsSetting>() as u32) | (1 << 16);

    /// The magic DRS setting that disables the game popup: a DWORD
    /// `0x10000000` under id `0x809D5F60` in the current profile.
    const POPUP_SETTING_ID: u32 = 0x809D_5F60;
    const POPUP_SETTING_VALUE: u32 = 0x10_000_000;
    const NVDRS_DWORD_TYPE: u32 = 0;
    const NVDRS_CURRENT_PROFILE_LOCATION: u32 = 0;

    /// The `nvapi_QueryInterface` interface ids (NVIDIA
    /// `nvapi_interface.h`), in resolve order, with their names. The ids are
    /// stored as a hex string and parsed at run time (see [`Nvapi::load`]):
    /// the driver withholds `NvAPI_DRS_SaveSettings` when an id reaches
    /// `nvapi_QueryInterface` as a compile-time constant, so the ids are
    /// resolved through a run-time heap buffer the compiler cannot fold to
    /// immediates.
    const INTERFACE_IDS_HEX: &str = "0150e828 d22bdd7e 0694d52e dad9cff8 375dbd6b fcbc7e14 cc176068 17093206 7e4a9a0b ed1f8c69 4347a9de 577dd202";
    const INTERFACE_NAMES: [&str; 12] = [
        "NvAPI_Initialize",
        "NvAPI_Unload",
        "NvAPI_DRS_CreateSession",
        "NvAPI_DRS_DestroySession",
        "NvAPI_DRS_LoadSettings",
        "NvAPI_DRS_SaveSettings",
        "NvAPI_DRS_CreateProfile",
        "NvAPI_DRS_DeleteProfile",
        "NvAPI_DRS_FindProfileByName",
        "NvAPI_DRS_GetApplicationInfo",
        "NvAPI_DRS_CreateApplication",
        "NvAPI_DRS_SetSetting",
    ];

    /// NVAPI status codes (raw values of `nvapi::Status`).
    const STATUS_OK: i32 = 0;
    const STATUS_PROFILE_NOT_FOUND: i32 = -163;
    const STATUS_EXECUTABLE_NOT_FOUND: i32 = -166;

    /// `nvapi_QueryInterface` as exported by the current drivers (old ABI):
    /// takes the interface id and returns the function pointer directly.
    type FnQueryInterface = unsafe extern "system" fn(id: u32) -> *mut c_void;
    type FnStatus = unsafe extern "system" fn() -> i32;
    type FnCreateSession = unsafe extern "system" fn(ph_session: *mut *mut c_void) -> i32;
    type FnSession = unsafe extern "system" fn(h_session: *mut c_void) -> i32;
    type FnCreateProfile = unsafe extern "system" fn(
        h_session: *mut c_void,
        p_profile: *mut NvdrsProfile,
        ph_profile: *mut *mut c_void,
    ) -> i32;
    type FnDeleteProfile =
        unsafe extern "system" fn(h_session: *mut c_void, h_profile: *mut c_void) -> i32;
    /// The C signature passes `NvAPI_UnicodeString` (a 4096-byte array) by
    /// value, which on the MS x64 ABI is a pointer to a caller-allocated
    /// buffer; `p_profile_name` is that pointer to a NUL-terminated UTF-16
    /// string.
    type FnFindProfileByName = unsafe extern "system" fn(
        h_session: *mut c_void,
        p_profile_name: *const u16,
        ph_profile: *mut *mut c_void,
    ) -> i32;
    /// `p_app_name` follows the same by-value string ABI as
    /// `FnFindProfileByName::p_profile_name`.
    type FnGetApplicationInfo = unsafe extern "system" fn(
        h_session: *mut c_void,
        h_profile: *mut c_void,
        p_app_name: *const u16,
        p_app: *mut NvdrsApplication,
    ) -> i32;
    type FnCreateApplication = unsafe extern "system" fn(
        h_session: *mut c_void,
        h_profile: *mut c_void,
        p_app: *mut NvdrsApplication,
    ) -> i32;
    type FnSetSetting = unsafe extern "system" fn(
        h_session: *mut c_void,
        h_profile: *mut c_void,
        p_setting: *mut NvdrsSetting,
    ) -> i32;

    /// The loaded `nvapi64.dll` and the NVAPI functions resolved through
    /// `nvapi_QueryInterface`.
    struct Nvapi {
        lib: HMODULE,
        initialize: FnStatus,
        unload: FnStatus,
        create_session: FnCreateSession,
        destroy_session: FnSession,
        load_settings: FnSession,
        save_settings: FnSession,
        create_profile: FnCreateProfile,
        delete_profile: FnDeleteProfile,
        find_profile_by_name: FnFindProfileByName,
        get_application_info: FnGetApplicationInfo,
        create_application: FnCreateApplication,
        set_setting: FnSetSetting,
    }

    impl Drop for Nvapi {
        fn drop(&mut self) {
            // SAFETY: `lib` was loaded by `LoadLibraryW` in `Nvapi::load` and
            // is still owned by this value.
            let _ = unsafe { FreeLibrary(self.lib) };
        }
    }

    impl Nvapi {
        /// Loads `nvapi64.dll` and resolves the 12 NVAPI/DRS functions needed
        /// for profile management.
        fn load() -> Result<Self, String> {
            // SAFETY: `w!`/`s!` produce valid NUL-terminated wide/ANSI
            // strings; the returned `HMODULE` is stored and released in
            // `Drop`.
            let lib = unsafe { LoadLibraryW(w!("nvapi64.dll")) }
                .map_err(|error| format!("failed to load nvapi64.dll: {error}"))?;
            // SAFETY: `lib` is a valid module handle; the result is `None`
            // when the export is missing.
            let query = unsafe { GetProcAddress(lib, s!("nvapi_QueryInterface")) };
            let Some(query) = query else {
                // SAFETY: `lib` was loaded above and is no longer needed.
                let _ = unsafe { FreeLibrary(lib) };
                return Err("nvapi64.dll does not export nvapi_QueryInterface".to_owned());
            };
            // SAFETY: `query` holds the address of the driver's
            // `nvapi_QueryInterface`; `transmute_copy` reinterprets the
            // same-size function pointer bit pattern with the correct
            // signature (the old ABI, where it returns the function pointer
            // directly).
            let query = unsafe { transmute_copy::<_, FnQueryInterface>(&query) };
            // The ids are parsed at run time into a heap buffer so they are
            // not compile-time constants in the `query` calls below; the
            // driver withholds `NvAPI_DRS_SaveSettings` for constant ids
            // (see `INTERFACE_IDS_HEX`).
            let ids: Vec<u32> = INTERFACE_IDS_HEX
                .split_whitespace()
                .map(|hex| u32::from_str_radix(hex, 16))
                .collect::<Result<_, _>>()
                .map_err(|error| {
                    // SAFETY: `lib` was loaded above and is no longer needed.
                    let _ = unsafe { FreeLibrary(lib) };
                    format!("internal error: malformed interface id table: {error}")
                })?;
            let resolve = |index: usize| -> Result<*mut c_void, String> {
                // SAFETY: `query` is the driver's `nvapi_QueryInterface`;
                // `ids[index]` is a valid interface id from NVIDIA's
                // `nvapi_interface.h`; a NULL result means the driver does
                // not implement the function.
                let pointer = unsafe { query(ids[index]) };
                if pointer.is_null() {
                    Err(format!(
                        "nvapi64.dll does not implement {}",
                        INTERFACE_NAMES[index]
                    ))
                } else {
                    Ok(pointer)
                }
            };
            let mut pointers = [ptr::null_mut::<c_void>(); 12];
            for (index, slot) in pointers.iter_mut().enumerate() {
                *slot = match resolve(index) {
                    Ok(pointer) => pointer,
                    Err(error) => {
                        // SAFETY: `lib` was loaded by `LoadLibraryW` above and
                        // is no longer needed.
                        let _ = unsafe { FreeLibrary(lib) };
                        return Err(error);
                    }
                };
            }
            // SAFETY: every value in `pointers` is a function address
            // obtained from the driver's `nvapi_QueryInterface`; each target
            // type is an `extern "system"` function pointer of the same size.
            Ok(Nvapi {
                lib,
                initialize: unsafe { transmute_copy(&pointers[0]) },
                unload: unsafe { transmute_copy(&pointers[1]) },
                create_session: unsafe { transmute_copy(&pointers[2]) },
                destroy_session: unsafe { transmute_copy(&pointers[3]) },
                load_settings: unsafe { transmute_copy(&pointers[4]) },
                save_settings: unsafe { transmute_copy(&pointers[5]) },
                create_profile: unsafe { transmute_copy(&pointers[6]) },
                delete_profile: unsafe { transmute_copy(&pointers[7]) },
                find_profile_by_name: unsafe { transmute_copy(&pointers[8]) },
                get_application_info: unsafe { transmute_copy(&pointers[9]) },
                create_application: unsafe { transmute_copy(&pointers[10]) },
                set_setting: unsafe { transmute_copy(&pointers[11]) },
            })
        }

        /// Initializes NVAPI and creates a DRS session; the returned guard
        /// destroys the session and unloads NVAPI when dropped.
        fn session(&self) -> Result<Session<'_>, String> {
            // SAFETY: `NvAPI_Initialize` takes no parameters; it ref-counts
            // NVAPI and is paired with `NvAPI_Unload` in `Session::drop`.
            let status = unsafe { (self.initialize)() };
            if status != STATUS_OK {
                return Err(format!(
                    "failed to initialize NVAPI: {}",
                    status_text(status)
                ));
            }
            let mut handle: *mut c_void = ptr::null_mut();
            // SAFETY: `handle` is a writable out-pointer that the driver
            // fills; on failure the status is checked and the handle is not
            // used.
            let status = unsafe { (self.create_session)(&mut handle) };
            if status != STATUS_OK || handle.is_null() {
                // SAFETY: paired with the `NvAPI_Initialize` above.
                let _ = unsafe { (self.unload)() };
                return Err(format!(
                    "failed to create a driver settings session: {}",
                    status_text(status)
                ));
            }
            Ok(Session { api: self, handle })
        }
    }

    /// A DRS session over [`Nvapi`]; destroying the session and unloading
    /// NVAPI happens on drop.
    struct Session<'a> {
        api: &'a Nvapi,
        handle: *mut c_void,
    }

    impl Drop for Session<'_> {
        fn drop(&mut self) {
            // SAFETY: `handle` is a live DRS session for as long as this guard
            // is alive; `NvAPI_Unload` is paired with the `NvAPI_Initialize`
            // in `Nvapi::session`.
            unsafe {
                let _ = (self.api.destroy_session)(self.handle);
                let _ = (self.api.unload)();
            }
        }
    }

    impl Session<'_> {
        fn load_settings(&self) -> Result<(), String> {
            // SAFETY: `handle` is a live session.
            let status = unsafe { (self.api.load_settings)(self.handle) };
            if status != STATUS_OK {
                return Err(format!(
                    "failed to load the driver settings: {}",
                    status_text(status)
                ));
            }
            Ok(())
        }

        fn save_settings(&self) -> Result<(), String> {
            // SAFETY: `handle` is a live session.
            let status = unsafe { (self.api.save_settings)(self.handle) };
            if status != STATUS_OK {
                return Err(format!(
                    "failed to save the driver settings: {}",
                    status_text(status)
                ));
            }
            Ok(())
        }

        /// Finds the profile by name, creating it when it does not exist yet.
        fn find_or_create_profile(&self) -> Result<*mut c_void, String> {
            let name = wide(super::PROFILE_NAME);
            let mut profile: *mut c_void = ptr::null_mut();
            // SAFETY: `handle` is a live session; the `NvAPI_UnicodeString`
            // parameter is by-value in C, which on x64 is a pointer to a
            // caller-allocated buffer — `name.as_ptr()` is such a pointer to a
            // valid NUL-terminated string; `profile` is a writable out-pointer.
            let status = unsafe {
                (self.api.find_profile_by_name)(self.handle, name.as_ptr(), &mut profile)
            };
            if status == STATUS_PROFILE_NOT_FOUND {
                let mut info = NvdrsProfile {
                    version: PROFILE_VERSION,
                    profile_name: [0; 2048],
                    gpu_support: 0,
                    is_predefined: 0,
                    num_of_apps: 0,
                    num_of_settings: 0,
                };
                copy_wide(&mut info.profile_name, &name);
                // SAFETY: `info` outlives the call and is zero-initialized
                // except for the version and name; the driver fills the rest.
                let status =
                    unsafe { (self.api.create_profile)(self.handle, &mut info, &mut profile) };
                if status != STATUS_OK || profile.is_null() {
                    return Err(format!(
                        "failed to create the {} profile: {}",
                        super::PROFILE_NAME,
                        status_text(status)
                    ));
                }
                return Ok(profile);
            }
            if status != STATUS_OK {
                return Err(format!(
                    "failed to find the {} profile: {}",
                    super::PROFILE_NAME,
                    status_text(status)
                ));
            }
            Ok(profile)
        }

        /// Registers [`super::EXE_NAME`] in the profile when it is not there
        /// yet.
        fn ensure_application(&self, profile: *mut c_void) -> Result<(), String> {
            let name = wide(super::EXE_NAME);
            let mut app = NvdrsApplication {
                version: APPLICATION_VERSION,
                is_predefined: 0,
                app_name: [0; 2048],
                user_friendly_name: [0; 2048],
                launcher: [0; 2048],
                file_in_folder: [0; 2048],
                flags: 0,
                command_line: [0; 2048],
            };
            copy_wide(&mut app.app_name, &name);
            // SAFETY: `handle`/`profile` are live; the `NvAPI_UnicodeString`
            // parameter is a by-value array, passed as a pointer to a
            // caller-allocated NUL-terminated string; `app` is a writable
            // out-pointer the driver fills on success.
            let status = unsafe {
                (self.api.get_application_info)(self.handle, profile, name.as_ptr(), &mut app)
            };
            if status == STATUS_EXECUTABLE_NOT_FOUND {
                // SAFETY: `app` is zero-initialized with the version and name
                // set; the driver overwrites it.
                let status =
                    unsafe { (self.api.create_application)(self.handle, profile, &mut app) };
                if status != STATUS_OK {
                    return Err(format!(
                        "failed to register {} in the {} profile: {}",
                        super::EXE_NAME,
                        super::PROFILE_NAME,
                        status_text(status)
                    ));
                }
            } else if status != STATUS_OK {
                return Err(format!(
                    "failed to query {} in the {} profile: {}",
                    super::EXE_NAME,
                    super::PROFILE_NAME,
                    status_text(status)
                ));
            }
            Ok(())
        }

        /// Sets the magic game-popup-disabling DWORD in the profile.
        fn set_popup_setting(&self, profile: *mut c_void) -> Result<(), String> {
            let mut setting = NvdrsSetting {
                version: SETTING_VERSION,
                setting_name: [0; 2048],
                setting_id: POPUP_SETTING_ID,
                setting_type: NVDRS_DWORD_TYPE,
                setting_location: NVDRS_CURRENT_PROFILE_LOCATION,
                is_current_predefined: 0,
                is_predefined_valid: 0,
                predefined: SettingValue { dword: 0 },
                current: SettingValue {
                    dword: POPUP_SETTING_VALUE,
                },
            };
            // SAFETY: `handle`/`profile` are live; `setting` is fully
            // initialized before the call and stays alive for its duration.
            let status = unsafe { (self.api.set_setting)(self.handle, profile, &mut setting) };
            if status != STATUS_OK {
                return Err(format!(
                    "failed to set the game popup setting: {}",
                    status_text(status)
                ));
            }
            Ok(())
        }
    }

    /// Adds the profile and registers the executable (see the module docs).
    pub(super) fn add_profile() -> Result<(), String> {
        let api = Nvapi::load()?;
        let session = api.session()?;
        session.load_settings()?;
        let profile = session.find_or_create_profile()?;
        session.ensure_application(profile)?;
        session.set_popup_setting(profile)?;
        session.save_settings()
    }

    /// Deletes the profile, treating a missing profile as success (see the
    /// module docs).
    pub(super) fn remove_profile() -> Result<(), String> {
        let api = Nvapi::load()?;
        let session = api.session()?;
        session.load_settings()?;
        let name = wide(super::PROFILE_NAME);
        let mut profile: *mut c_void = ptr::null_mut();
        // SAFETY: same by-value string contract as
        // `Session::find_or_create_profile`.
        let status = unsafe {
            (session.api.find_profile_by_name)(session.handle, name.as_ptr(), &mut profile)
        };
        if status == STATUS_PROFILE_NOT_FOUND {
            return Ok(());
        }
        if status != STATUS_OK {
            return Err(format!(
                "failed to find the {} profile: {}",
                super::PROFILE_NAME,
                status_text(status)
            ));
        }
        // SAFETY: `profile` is a live profile handle from
        // `FindProfileByName`; it stays valid until the session is dropped.
        let status = unsafe { (session.api.delete_profile)(session.handle, profile) };
        if status != STATUS_OK {
            return Err(format!(
                "failed to delete the {} profile: {}",
                super::PROFILE_NAME,
                status_text(status)
            ));
        }
        session.save_settings()
    }

    /// Runs a profile operation, re-launching the app elevated (a UAC prompt)
    /// when this process is not elevated: the machine-wide (per-machine) DRS
    /// store requires administrator rights to write. `add` selects the add vs.
    /// remove operation and the CLI flag passed to the elevated copy.
    pub(super) fn run_interactive(add: bool) -> Result<(), String> {
        if is_elevated() {
            return if add { add_profile() } else { remove_profile() };
        }
        let flag = if add {
            "--add-nvidia-app-profile"
        } else {
            "--delete-nvidia-app-profile"
        };
        relaunch_self_elevated(flag)
    }

    /// Whether the current process token is elevated.
    fn is_elevated() -> bool {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        // SAFETY: opens the current process token for a read-only query and
        // reads a fixed-size `TOKEN_ELEVATION` struct; the handle is always
        // closed, and a token that cannot be opened simply reports non-elevated.
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
            let mut length = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(ptr::addr_of_mut!(elevation).cast()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut length,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// Re-launches this executable elevated (the `runas` verb) with the given
    /// CLI flag, waits for the elevated copy to finish, and maps its exit code
    /// (0 = success, non-zero = failure) to the result. The `nShow` field is
    /// left at its `Default` (SW_HIDE) so no window is shown for the
    /// background operation.
    fn relaunch_self_elevated(flag: &str) -> Result<(), String> {
        use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        use windows::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows::core::PCWSTR;

        /// Bounded wait: a hung elevated copy must not keep the settings
        /// link disabled for the whole session; the operation is idempotent,
        /// so a timed-out run can simply be retried.
        const TIMEOUT_MS: u32 = 300_000;

        let exe = std::env::current_exe()
            .map_err(|error| format!("failed to resolve the executable path: {error}"))?;
        let verb = w!("runas");
        let file: Vec<u16> = exe
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let param: Vec<u16> = flag.encode_utf16().chain(std::iter::once(0)).collect();

        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            ..Default::default()
        };
        info.lpVerb = PCWSTR(verb.as_ptr());
        info.lpFile = PCWSTR(file.as_ptr());
        info.lpParameters = PCWSTR(param.as_ptr());

        // SAFETY: `info` is fully initialized and its `lpVerb`/`lpFile`/
        // `lpParameters` fields point at buffers that stay alive across the
        // call; with `SEE_MASK_NOCLOSEPROCESS`, `info.hProcess` is a valid
        // process handle on success that we close below.
        let result = unsafe { ShellExecuteExW(&mut info) };
        let Ok(()) = result else {
            return Err(
                "the NVIDIA profile change was not started; administrator rights are required"
                    .to_owned(),
            );
        };
        // SAFETY: `info.hProcess` is a live handle opened via NOCLOSEPROCESS;
        // it is closed after the exit code is read.
        let (wait, exit_code) = unsafe {
            let wait = WaitForSingleObject(info.hProcess, TIMEOUT_MS);
            let mut code = 0u32;
            let ok = GetExitCodeProcess(info.hProcess, &mut code).is_ok();
            let _ = CloseHandle(info.hProcess);
            (wait, if ok { code } else { 1 })
        };
        if wait == WAIT_TIMEOUT {
            return Err("the elevated NVIDIA profile operation timed out".to_owned());
        }
        if exit_code == 0 {
            Ok(())
        } else {
            Err(format!(
                "the elevated NVIDIA profile operation failed with exit code {exit_code}"
            ))
        }
    }

    /// Names an NVAPI status code via the `nvapi` crate's `Status` enum;
    /// unknown codes are reported with the raw value.
    fn status_text(raw: i32) -> String {
        match Status::from_raw(raw) {
            Ok(status) => format!("{status}"),
            Err(_) => format!("NvAPI status {raw}"),
        }
    }

    /// Encodes `s` as a NUL-terminated UTF-16 buffer.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Copies a NUL-terminated UTF-16 buffer into an `NvAPI_UnicodeString`
    /// field.
    fn copy_wide(dst: &mut NvApiUnicodeString, src: &[u16]) {
        let n = src.len().min(dst.len());
        dst[..n].copy_from_slice(&src[..n]);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    mod windows {
        use super::super::win;
        use std::mem::{align_of, size_of};

        /// The DRS struct layouts must stay in sync with the NVIDIA
        /// `nvapi.h` definitions as compiled with MSVC x64 (verified against
        /// the official header: profile 4116/4, application 20492/4, setting
        /// 12320/4).
        #[test]
        fn drs_struct_layouts_match_the_nvidia_header() {
            assert_eq!(size_of::<win::NvdrsProfile>(), 4116);
            assert_eq!(align_of::<win::NvdrsProfile>(), 4);
            assert_eq!(size_of::<win::NvdrsApplication>(), 20492);
            assert_eq!(align_of::<win::NvdrsApplication>(), 4);
            assert_eq!(size_of::<win::NvdrsSetting>(), 12320);
            assert_eq!(align_of::<win::NvdrsSetting>(), 4);
            assert_eq!(win::PROFILE_VERSION, 0x0001_1014);
            assert_eq!(win::APPLICATION_VERSION, 0x0004_500C);
            assert_eq!(win::SETTING_VERSION, 0x0001_3020);
        }
    }

    /// On non-Windows platforms the operations fail with a clear message.
    #[cfg(not(windows))]
    #[test]
    fn profile_operations_are_unsupported_off_windows() {
        assert!(super::add_profile().is_err());
        assert!(super::remove_profile().is_err());
    }
}
