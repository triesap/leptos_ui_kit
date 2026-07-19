#![allow(unsafe_code)]

use std::{
    ffi::{OsStr, c_void},
    fs::File,
    io,
    mem::{align_of, offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    ptr,
    sync::Arc,
};

use fs_at::{OpenOptions, os::windows::OpenOptionsExt as _};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile, NtFlushBuffersFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_ALREADY_EXISTS, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FILE_EXISTS,
            ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SAME_DEVICE,
            ERROR_NOT_SUPPORTED, HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE,
            RtlNtStatusToDosError, UNICODE_STRING,
        },
        Storage::FileSystem::{
            DELETE, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_FLAG_DELETE,
            FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_REMOTE_PROTOCOL_INFO, FILE_RENAME_INFO,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
            FileAttributeTagInfo, FileDispositionInfoEx, FileIdInfo, FileRemoteProtocolInfo,
            FileRenameInfoEx, FileStandardInfo, GetFileInformationByHandleEx,
            GetVolumeInformationByHandleW, ReOpenFile, SYNCHRONIZE, SetFileInformationByHandle,
        },
        System::{
            IO::IO_STATUS_BLOCK,
            SystemServices::{FILE_SUPPORTS_HARD_LINKS, FILE_SUPPORTS_REPARSE_POINTS},
            WindowsProgramming::{
                FILE_CREATED, FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS,
            },
        },
    },
};

use crate::{
    AdoptedRootDirectory, AdoptedRootInner, AdoptionError, CapabilityAccess, CapabilityInner,
    ChildBinding, DirectoryCapability, DurabilityFailure, DurableMutation, FileIdentity,
    MovePolicy, MutationFailure, NamespacePostcondition, ObjectCapability, ObjectInfo, ObjectKind,
    QualifiedVolume, ReplacementCapabilities, ReplacementPolicy, UnverifiedObjectCapability,
    UnverifiedReplacementCapabilities, VerifiedMutation, VolumeCapabilities, VolumeQualification,
    access_required, alias_rejected, binding_changed,
};

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const REOPEN_FLAGS: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;

pub(super) fn validate_direct_name(name: &OsStr) -> io::Result<()> {
    let text = name.to_str().ok_or_else(crate::invalid_direct_name)?;
    crate::validate_windows_name_text(text)
}

pub(super) fn adopt_root_directory(original: File) -> Result<AdoptedRootDirectory, AdoptionError> {
    let original_info = match inspect_exact_file(&original, ObjectKind::Directory) {
        Ok(info) => info,
        Err(error) => {
            return Err(AdoptionError {
                _handle: original,
                error,
            });
        }
    };

    // The first reopen requests no DELETE access, so an existing cap-std
    // directory handle which omitted FILE_SHARE_DELETE does not block it. The
    // returned bridge itself shares delete access.
    let bridge = match reopen_exact(&original, CapabilityAccess::Inspect) {
        Ok(bridge) => bridge,
        Err(error) => {
            return Err(AdoptionError {
                _handle: original,
                error,
            });
        }
    };
    if let Err(error) = require_same_observation(
        inspect_exact_file(&bridge, ObjectKind::Directory),
        original_info.identity,
        ObjectKind::Directory,
    ) {
        return Err(AdoptionError {
            _handle: bridge,
            error,
        });
    }

    // Close the possibly no-share-delete ingress before requesting DELETE on a
    // second exact reopen.
    drop(original);
    let durable = match reopen_exact(&bridge, CapabilityAccess::Mutation) {
        Ok(durable) => durable,
        Err(error) => {
            return Err(AdoptionError {
                _handle: bridge,
                error,
            });
        }
    };
    let durable_info = match require_same_observation(
        inspect_exact_file(&durable, ObjectKind::Directory),
        original_info.identity,
        ObjectKind::Directory,
    ) {
        Ok(info) => info,
        Err(error) => {
            return Err(AdoptionError {
                _handle: durable,
                error,
            });
        }
    };
    drop(bridge);

    Ok(AdoptedRootDirectory(AdoptedRootInner {
        file: durable,
        info: durable_info,
    }))
}

pub(super) fn refresh_verified(capability: &CapabilityInner) -> io::Result<ObjectInfo> {
    require_same_observation(
        inspect_exact_file(&capability.file, capability.info.kind),
        capability.info.identity,
        capability.info.kind,
    )
}

pub(super) fn open_child_nofollow(
    parent: &DirectoryCapability,
    name: &OsStr,
    expected_kind: ObjectKind,
    access: CapabilityAccess,
) -> io::Result<ObjectCapability> {
    let capability = open_child_any(parent, name, access)?;
    if capability.kind() != expected_kind {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the direct child kind did not match the authorized kind",
        ));
    }
    Ok(capability)
}

pub(super) fn create_directory_nofollow(
    parent: &DirectoryCapability,
    name: &OsStr,
) -> Result<VerifiedMutation<DirectoryCapability>, MutationFailure<(), UnverifiedObjectCapability>>
{
    if let Err(error) = require_mutation_parent(parent, "directory creation") {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(()),
        });
    }
    if let Err(error) = refresh_verified(&parent.0) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(()),
        });
    }
    match try_open_child_any(parent, name, CapabilityAccess::Inspect) {
        Ok(Some(_)) => {
            return Err(MutationFailure::NotMutated {
                error: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the direct child already exists",
                ),
                capabilities: Box::new(()),
            });
        }
        Ok(None) => {}
        Err(error) => {
            return Err(MutationFailure::NotMutated {
                error,
                capabilities: Box::new(()),
            });
        }
    }

    let binding = Some(ChildBinding {
        parent_identity: parent.identity(),
        name: name.to_os_string(),
    });
    let created = match create_directory_handle(parent, name) {
        Ok(created) => created,
        Err(CreateHandleFailure::NotMutated(error)) => {
            return Err(MutationFailure::NotMutated {
                error,
                capabilities: Box::new(()),
            });
        }
        Err(CreateHandleFailure::MutatedUnverified { handle, error }) => {
            return Err(MutationFailure::MutatedUnverified {
                error,
                capabilities: Box::new(UnverifiedObjectCapability {
                    file: handle,
                    expected_kind: ObjectKind::Directory,
                    access: CapabilityAccess::Mutation,
                    binding,
                    last_observation: None,
                    qualification: Arc::clone(&parent.0.qualification),
                }),
            });
        }
    };
    let created_info = match inspect_exact_file(&created, ObjectKind::Directory) {
        Ok(info) => info,
        Err(error) => {
            return Err(MutationFailure::MutatedUnverified {
                error,
                capabilities: Box::new(UnverifiedObjectCapability {
                    file: created,
                    expected_kind: ObjectKind::Directory,
                    access: CapabilityAccess::Mutation,
                    binding,
                    last_observation: None,
                    qualification: Arc::clone(&parent.0.qualification),
                }),
            });
        }
    };
    let mut capability = ObjectCapability(CapabilityInner {
        file: created,
        info: created_info,
        access: CapabilityAccess::Mutation,
        binding,
        qualification: Arc::clone(&parent.0.qualification),
    });

    let verification = (|| {
        let rebound = open_child_any(parent, name, CapabilityAccess::Inspect)?;
        require_identity(
            &rebound.0.info,
            created_info.identity,
            ObjectKind::Directory,
        )?;
        refresh_verified(&parent.0)?;
        let current = inspect_exact_file(&capability.0.file, ObjectKind::Directory)?;
        require_identity(&current, created_info.identity, ObjectKind::Directory)?;
        capability.0.info = current;
        Ok(())
    })();
    if let Err(error) = verification {
        return Err(MutationFailure::MutatedUnverified {
            error,
            capabilities: Box::new(capability.into_unverified()),
        });
    }

    Ok(VerifiedMutation::new(
        DirectoryCapability(capability.0),
        Arc::clone(&parent.0.qualification),
        [parent.identity()],
        [NamespacePostcondition::present(parent, name, created_info)],
    ))
}

pub(super) fn move_noreplace(
    mut source: ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    policy: MovePolicy,
) -> Result<
    VerifiedMutation<ObjectCapability>,
    MutationFailure<ObjectCapability, UnverifiedObjectCapability>,
> {
    if let Err(error) = preflight_move(&source, source_parent, target_parent, policy) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(source),
        });
    }
    let source_name = source
        .name()
        .expect("preflight requires a direct-child source binding")
        .to_os_string();
    match try_open_child_any(target_parent, target_name, CapabilityAccess::Inspect) {
        Ok(Some(target)) => {
            let error = if target.identity() == source.identity() {
                alias_rejected("no-replace move")
            } else {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the destination already exists",
                )
            };
            return Err(MutationFailure::NotMutated {
                error,
                capabilities: Box::new(source),
            });
        }
        Ok(None) => {}
        Err(error) => {
            return Err(MutationFailure::NotMutated {
                error,
                capabilities: Box::new(source),
            });
        }
    }

    if let Err(mut error) = rename_by_handle(&source.0.file, target_parent, target_name, 0) {
        if error.kind() == io::ErrorKind::AlreadyExists
            && let Ok(Some(target)) =
                try_open_child_any(target_parent, target_name, CapabilityAccess::Inspect)
            && target.identity() == source.identity()
        {
            error = alias_rejected("no-replace move");
        }
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(source),
        });
    }

    source.0.binding = Some(ChildBinding {
        parent_identity: target_parent.identity(),
        name: target_name.to_os_string(),
    });
    let verification = verify_moved_capability(
        &mut source,
        source_parent,
        &source_name,
        target_parent,
        target_name,
        policy.source_before(),
    );
    if let Err(error) = verification {
        return Err(MutationFailure::MutatedUnverified {
            error,
            capabilities: Box::new(source.into_unverified()),
        });
    }
    Ok(VerifiedMutation::new(
        source,
        Arc::clone(&target_parent.0.qualification),
        [source_parent.identity(), target_parent.identity()],
        [
            NamespacePostcondition::absent(source_parent, &source_name),
            NamespacePostcondition::present(target_parent, target_name, policy.source_before()),
        ],
    ))
}

pub(super) fn replace_exact(
    mut source: ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    mut target: ObjectCapability,
    policy: ReplacementPolicy,
) -> Result<
    VerifiedMutation<ObjectCapability>,
    MutationFailure<ReplacementCapabilities, UnverifiedReplacementCapabilities>,
> {
    if let Err(error) = preflight_replace(
        &source,
        source_parent,
        target_parent,
        target_name,
        &target,
        policy,
    ) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(ReplacementCapabilities { source, target }),
        });
    }
    let source_name = source
        .name()
        .expect("preflight requires a direct-child source binding")
        .to_os_string();

    if let Err(error) = rename_by_handle(
        &source.0.file,
        target_parent,
        target_name,
        FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS,
    ) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(ReplacementCapabilities { source, target }),
        });
    }

    source.0.binding = Some(ChildBinding {
        parent_identity: target_parent.identity(),
        name: target_name.to_os_string(),
    });
    // The old target handle remains exact, but its former name binding has
    // been replaced and must never be represented as verified.
    target.0.binding = None;
    let verification = verify_moved_capability(
        &mut source,
        source_parent,
        &source_name,
        target_parent,
        target_name,
        policy.source_before(),
    );
    if let Err(error) = verification {
        return Err(MutationFailure::MutatedUnverified {
            error,
            capabilities: Box::new(UnverifiedReplacementCapabilities {
                source: source.into_unverified(),
                target: target.into_unverified(),
            }),
        });
    }

    drop(target);
    Ok(VerifiedMutation::new(
        source,
        Arc::clone(&target_parent.0.qualification),
        [source_parent.identity(), target_parent.identity()],
        [
            NamespacePostcondition::absent(source_parent, &source_name),
            NamespacePostcondition::present(target_parent, target_name, policy.source_before()),
        ],
    ))
}

pub(super) fn delete_exact(
    parent: &DirectoryCapability,
    object: ObjectCapability,
) -> Result<VerifiedMutation<()>, MutationFailure<ObjectCapability, UnverifiedObjectCapability>> {
    if let Err(error) = preflight_delete(parent, &object) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(object),
        });
    }
    let name = object
        .name()
        .expect("preflight requires a direct-child binding")
        .to_os_string();
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: the opaque capability guarantees DELETE access and a live exact
    // handle. `disposition` is a correctly sized repr(C) value borrowed only
    // for this synchronous call.
    let result = unsafe {
        SetFileInformationByHandle(
            raw_handle(&object.0.file),
            FileDispositionInfoEx,
            ptr::from_ref(&disposition).cast::<c_void>(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if result == 0 {
        return Err(MutationFailure::NotMutated {
            error: map_disposition_error(io::Error::last_os_error()),
            capabilities: Box::new(object),
        });
    }

    let verification = match try_open_child_any(parent, &name, CapabilityAccess::Inspect) {
        Ok(None) => refresh_verified(&parent.0).map(|_| ()),
        Ok(Some(_)) => Err(binding_changed()),
        Err(error) => Err(error),
    };
    if let Err(error) = verification {
        return Err(MutationFailure::MutatedUnverified {
            error,
            capabilities: Box::new(object.into_unverified()),
        });
    }

    drop(object);
    Ok(VerifiedMutation::new(
        (),
        Arc::clone(&parent.0.qualification),
        [parent.identity()],
        [NamespacePostcondition::absent(parent, &name)],
    ))
}

pub(super) fn sync_directory(directory: &DirectoryCapability) -> io::Result<()> {
    require_mutation_parent(directory, "directory durability")?;
    refresh_verified(&directory.0)?;
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: the opaque capability guarantees synchronization and write
    // rights. The exact directory handle stays live, and `status_block` is a
    // valid writable buffer not retained by this synchronous call.
    let status = unsafe { NtFlushBuffersFile(raw_handle(&directory.0.file), &mut status_block) };
    if status < 0 {
        return Err(map_flush_error(ntstatus_to_io(status)));
    }
    refresh_verified(&directory.0)?;
    Ok(())
}

pub(super) fn sync_mutation_parents<T>(
    mutation: VerifiedMutation<T>,
    parents: &[&DirectoryCapability],
) -> Result<DurableMutation<T>, DurabilityFailure<T>> {
    let sync_result = (|| {
        if parents.len() != mutation.required_parent_identities.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the supplied parent set does not match the mutation durability obligation",
            ));
        }

        let mut observed = Vec::with_capacity(parents.len());
        for parent in parents {
            require_mutation_parent(parent, "mutation parent durability")?;
            require_shared_qualification(&mutation.qualification, &parent.0.qualification)?;
            let info = refresh_verified(&parent.0)?;
            if !mutation.required_parent_identities.contains(&info.identity)
                || observed.contains(&info.identity)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the supplied parent set does not exactly match the mutation durability obligation",
                ));
            }
            observed.push(info.identity);
        }

        verify_namespace_postconditions(&mutation, parents)?;
        for expected in &mutation.required_parent_identities {
            let parent = parents
                .iter()
                .copied()
                .find(|parent| parent.identity() == *expected)
                .ok_or_else(|| {
                    io::Error::other("the validated parent set lost a required durability identity")
                })?;
            sync_directory(parent)?;
        }
        verify_namespace_postconditions(&mutation, parents)?;
        Ok(())
    })();

    match sync_result {
        Ok(()) => Ok(DurableMutation {
            value: mutation.value,
        }),
        Err(error) => Err(DurabilityFailure { error, mutation }),
    }
}

fn verify_namespace_postconditions<T>(
    mutation: &VerifiedMutation<T>,
    parents: &[&DirectoryCapability],
) -> io::Result<()> {
    for postcondition in &mutation.postconditions {
        let parent_identity = match postcondition {
            NamespacePostcondition::Present {
                parent_identity, ..
            }
            | NamespacePostcondition::Absent {
                parent_identity, ..
            } => *parent_identity,
        };
        let parent = parents
            .iter()
            .copied()
            .find(|parent| parent.identity() == parent_identity)
            .ok_or_else(|| {
                io::Error::other(
                    "a namespace postcondition was not bound to a required durability parent",
                )
            })?;
        match postcondition {
            NamespacePostcondition::Present { name, expected, .. } => {
                let observed = open_child_any(parent, name, CapabilityAccess::Inspect)?;
                require_exact_policy_observation(observed.info(), *expected)?;
            }
            NamespacePostcondition::Absent { name, .. } => {
                if try_open_child_any(parent, name, CapabilityAccess::Inspect)?.is_some() {
                    return Err(binding_changed());
                }
            }
        }
    }
    Ok(())
}

pub(super) fn probe_volume(root: AdoptedRootDirectory) -> io::Result<QualifiedVolume> {
    let root_info = require_same_observation(
        inspect_exact_file(&root.0.file, ObjectKind::Directory),
        root.0.info.identity,
        ObjectKind::Directory,
    )?;
    if is_remote(&root.0.file)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "remote filesystems are outside the transaction durability contract",
        ));
    }

    let mut maximum_component_length = 0;
    let mut flags = 0;
    let mut file_system_name = [0_u16; 64];
    // SAFETY: the exact root handle remains live. Optional outputs are null,
    // scalar outputs are valid, and the UTF-16 buffer length is exact.
    let result = unsafe {
        GetVolumeInformationByHandleW(
            raw_handle(&root.0.file),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &mut maximum_component_length,
            &mut flags,
            file_system_name.as_mut_ptr(),
            file_system_name.len() as u32,
        )
    };
    if result == 0 {
        return Err(map_volume_error(io::Error::last_os_error()));
    }
    let name_end = file_system_name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(file_system_name.len());
    let file_system_name = String::from_utf16(&file_system_name[..name_end]).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the filesystem name was not valid UTF-16",
        )
    })?;
    if maximum_component_length == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the transaction filesystem reported no direct-component capacity",
        ));
    }
    if !file_system_name.eq_ignore_ascii_case("NTFS")
        && !file_system_name.eq_ignore_ascii_case("ReFS")
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported transaction filesystem {file_system_name:?}"),
        ));
    }
    let supports_hard_links = flags & FILE_SUPPORTS_HARD_LINKS != 0;
    if !supports_hard_links {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the transaction filesystem does not report hard-link support",
        ));
    }
    let supports_reparse_points = flags & FILE_SUPPORTS_REPARSE_POINTS != 0;
    if !supports_reparse_points {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the transaction filesystem does not report reparse-point support",
        ));
    }

    let capabilities = VolumeCapabilities {
        root_identity: root_info.identity,
        file_system_name,
        maximum_component_length,
        supports_hard_links,
        supports_reparse_points,
    };
    let qualification = Arc::new(VolumeQualification { capabilities });
    Ok(QualifiedVolume {
        root: DirectoryCapability(CapabilityInner {
            file: root.0.file,
            info: root_info,
            access: CapabilityAccess::Mutation,
            binding: None,
            qualification,
        }),
    })
}

fn preflight_move(
    source: &ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    policy: MovePolicy,
) -> io::Result<()> {
    require_mutation_object(source, "no-replace move")?;
    require_mutation_parent(source_parent, "no-replace move")?;
    require_mutation_parent(target_parent, "no-replace move")?;
    require_shared_qualification(&source.0.qualification, &source_parent.0.qualification)?;
    require_shared_qualification(
        &source_parent.0.qualification,
        &target_parent.0.qualification,
    )?;
    let source_name = source.name().ok_or_else(binding_changed)?;
    if !source.binding_matches(source_parent, source_name) {
        return Err(binding_changed());
    }
    if policy.source_before() != source.info() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the move policy does not match the certified source capability",
        ));
    }
    require_exact_move_state(policy.source_before())?;
    let source_fresh = refresh_verified(&source.0)?;
    require_exact_policy_observation(source_fresh, policy.source_before())?;
    let rebound = open_child_any(source_parent, source_name, CapabilityAccess::Inspect)?;
    require_identity(&rebound.0.info, source.identity(), source.kind())?;
    require_exact_policy_observation(rebound.info(), policy.source_before())?;
    refresh_verified(&source_parent.0)?;
    refresh_verified(&target_parent.0)?;
    ensure_same_volume(source.identity(), target_parent.identity())?;
    ensure_not_alias(
        source.identity(),
        target_parent.identity(),
        "no-replace move",
    )
}

fn preflight_replace(
    source: &ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    target: &ObjectCapability,
    policy: ReplacementPolicy,
) -> io::Result<()> {
    require_mutation_object(source, "exact replacement")?;
    require_mutation_parent(source_parent, "exact replacement")?;
    require_mutation_parent(target_parent, "exact replacement")?;
    require_shared_qualification(&source.0.qualification, &source_parent.0.qualification)?;
    require_shared_qualification(
        &source_parent.0.qualification,
        &target_parent.0.qualification,
    )?;
    require_shared_qualification(&target.0.qualification, &target_parent.0.qualification)?;
    if source.kind() != target.kind() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "replacement source and target kinds differ",
        ));
    }
    if !target.binding_matches(target_parent, target_name) {
        return Err(binding_changed());
    }
    let source_name = source.name().ok_or_else(binding_changed)?;
    if !source.binding_matches(source_parent, source_name)
        || (source_parent.identity() == target_parent.identity() && source_name == target_name)
    {
        return Err(binding_changed());
    }
    if policy.source_before() != source.info() || policy.target_before() != target.info() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the replacement policy does not match the certified capabilities",
        ));
    }
    if policy.source_before().link_count == 0 || policy.target_before().link_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "replacement policy link counts must be nonzero",
        ));
    }
    if policy.target_before().readonly {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a readonly replacement target is outside transaction authority",
        ));
    }
    let source_fresh = refresh_verified(&source.0)?;
    let target_fresh = refresh_verified(&target.0)?;
    if target_fresh.readonly {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a readonly replacement target is outside transaction authority",
        ));
    }
    require_exact_policy_observation(source_fresh, policy.source_before())?;
    require_exact_policy_observation(target_fresh, policy.target_before())?;
    refresh_verified(&source_parent.0)?;
    refresh_verified(&target_parent.0)?;
    ensure_same_volume(source.identity(), target_parent.identity())?;
    ensure_same_volume(source.identity(), target.identity())?;
    ensure_not_alias(source.identity(), target.identity(), "exact replacement")?;
    ensure_not_alias(
        source.identity(),
        target_parent.identity(),
        "exact replacement",
    )?;
    let rebound_source = open_child_any(source_parent, source_name, CapabilityAccess::Inspect)?;
    require_identity(&rebound_source.0.info, source.identity(), source.kind())?;
    require_exact_policy_observation(rebound_source.info(), policy.source_before())?;
    let rebound = open_child_any(target_parent, target_name, CapabilityAccess::Inspect)?;
    if rebound.identity() == source.identity() {
        return Err(alias_rejected("exact replacement"));
    }
    require_identity(&rebound.0.info, target.identity(), target.kind())?;
    require_exact_policy_observation(rebound.info(), policy.target_before())?;
    Ok(())
}

fn preflight_delete(parent: &DirectoryCapability, object: &ObjectCapability) -> io::Result<()> {
    require_mutation_object(object, "exact deletion")?;
    require_mutation_parent(parent, "exact deletion")?;
    require_shared_qualification(&object.0.qualification, &parent.0.qualification)?;
    if !object
        .name()
        .is_some_and(|name| object.binding_matches(parent, name))
    {
        return Err(binding_changed());
    }
    let certified = object.info();
    let object_fresh = refresh_verified(&object.0)?;
    require_exact_delete_state(object_fresh)?;
    require_exact_policy_observation(object_fresh, certified)?;
    refresh_verified(&parent.0)?;
    ensure_not_alias(object.identity(), parent.identity(), "exact deletion")?;
    let rebound = open_child_any(
        parent,
        object.name().expect("binding checked above"),
        CapabilityAccess::Inspect,
    )?;
    require_identity(&rebound.0.info, object.identity(), object.kind())?;
    require_exact_delete_state(rebound.info())?;
    require_exact_policy_observation(rebound.info(), object_fresh)?;
    Ok(())
}

fn require_exact_policy_observation(observed: ObjectInfo, expected: ObjectInfo) -> io::Result<()> {
    if observed != expected {
        return Err(observation_changed());
    }
    Ok(())
}

fn require_exact_delete_state(info: ObjectInfo) -> io::Result<()> {
    if info.readonly {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "readonly objects are outside exact-deletion authority",
        ));
    }
    if info.kind == ObjectKind::RegularFile && info.link_count != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exact regular-file deletion requires one and only one hard link",
        ));
    }
    Ok(())
}

fn require_exact_move_state(info: ObjectInfo) -> io::Result<()> {
    if info.link_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exact movement requires a nonzero link count",
        ));
    }
    if info.kind == ObjectKind::RegularFile && info.link_count != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exact regular-file movement requires one and only one hard link",
        ));
    }
    Ok(())
}

fn verify_moved_capability(
    source: &mut ObjectCapability,
    source_parent: &DirectoryCapability,
    source_name: &OsStr,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    expected: ObjectInfo,
) -> io::Result<()> {
    let current = inspect_exact_file(&source.0.file, source.kind())?;
    require_identity(&current, source.identity(), source.kind())?;
    require_exact_policy_observation(current, expected)?;
    let rebound = open_child_any(target_parent, target_name, CapabilityAccess::Inspect)?;
    require_identity(&rebound.0.info, source.identity(), source.kind())?;
    require_exact_policy_observation(rebound.info(), expected)?;
    match try_open_child_any(source_parent, source_name, CapabilityAccess::Inspect)? {
        None => {}
        Some(_) => return Err(binding_changed()),
    }
    refresh_verified(&source_parent.0)?;
    refresh_verified(&target_parent.0)?;
    source.0.info = current;
    Ok(())
}

fn open_child_any(
    parent: &DirectoryCapability,
    name: &OsStr,
    access: CapabilityAccess,
) -> io::Result<ObjectCapability> {
    refresh_verified(&parent.0)?;
    let mut options = OpenOptions::default();
    options.desired_access(access_mask(access));
    options.create_options(FILE_OPEN_REPARSE_POINT);
    options.object_attributes(OBJ_CASE_INSENSITIVE);
    options.follow(false);
    let opened = options
        .open_at(&parent.0.file, name)
        .map_err(map_open_error)?;
    let kind = query_kind(&opened)?;
    let info = inspect_exact_file(&opened, kind)?;
    Ok(ObjectCapability(CapabilityInner {
        file: opened,
        info,
        access,
        binding: Some(ChildBinding {
            parent_identity: parent.identity(),
            name: name.to_os_string(),
        }),
        qualification: Arc::clone(&parent.0.qualification),
    }))
}

fn try_open_child_any(
    parent: &DirectoryCapability,
    name: &OsStr,
    access: CapabilityAccess,
) -> io::Result<Option<ObjectCapability>> {
    match open_child_any(parent, name, access) {
        Ok(capability) => Ok(Some(capability)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

enum CreateHandleFailure {
    NotMutated(io::Error),
    MutatedUnverified { handle: File, error: io::Error },
}

fn create_directory_handle(
    parent: &DirectoryCapability,
    name: &OsStr,
) -> Result<File, CreateHandleFailure> {
    let mut encoded = name.encode_wide().collect::<Vec<_>>();
    let byte_len = encoded
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| {
            CreateHandleFailure::NotMutated(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the direct child name exceeds the NT Unicode string limit",
            ))
        })?;
    let unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: encoded.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: raw_handle(&parent.0.file),
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: all pointers refer to initialized repr(C) values that outlive the
    // synchronous call. The name is a validated direct component resolved only
    // relative to the exact parent handle. FILE_CREATE is atomic no-replace,
    // and the requested access/share/options establish the capability rights.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access_mask(CapabilityAccess::Mutation),
            &object_attributes,
            &mut status_block,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            SHARE_ALL,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(CreateHandleFailure::NotMutated(map_create_error(
            ntstatus_to_io(status),
        )));
    }
    // SAFETY: the NtCreateFile contract guarantees that a nonnegative status
    // returns a valid newly owned HANDLE through its output parameter. No
    // other Rust value owns it, and File closes it exactly once.
    let file = unsafe { File::from_raw_handle(handle.cast()) };
    if status_block.Information != FILE_CREATED as usize {
        return Err(CreateHandleFailure::MutatedUnverified {
            handle: file,
            error: io::Error::other(format!(
                "FILE_CREATE returned unexpected information {}",
                status_block.Information
            )),
        });
    }
    Ok(file)
}

fn reopen_exact(original: &File, access: CapabilityAccess) -> io::Result<File> {
    // SAFETY: the original handle is live. ReOpenFile returns a distinct owned
    // handle, requests explicit share-delete and no-follow directory semantics,
    // and retains no borrowed pointer.
    let handle = unsafe {
        ReOpenFile(
            raw_handle(original),
            access_mask(access),
            SHARE_ALL,
            REOPEN_FLAGS,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(map_reopen_error(io::Error::last_os_error()));
    }
    // SAFETY: successful ReOpenFile returned a new owned HANDLE.
    Ok(unsafe { File::from_raw_handle(handle.cast()) })
}

fn inspect_exact_file(object: &File, expected_kind: ObjectKind) -> io::Result<ObjectInfo> {
    let identity_before = query_identity(object)?;
    if identity_before.volume_serial_number == 0
        || identity_before.file_id.iter().all(|byte| *byte == 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the filesystem did not provide a complete nonzero FILE_ID_INFO identity",
        ));
    }
    let attributes: FILE_ATTRIBUTE_TAG_INFO =
        query_file_info(object, FileAttributeTagInfo, QueryKind::AttributeTag)?;
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reparse points are outside transaction authority",
        ));
    }
    let standard: FILE_STANDARD_INFO =
        query_file_info(object, FileStandardInfo, QueryKind::Standard)?;
    if standard.DeletePending {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the exact object is already delete-pending",
        ));
    }
    let kind = standard_kind(&standard);
    if kind != expected_kind {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the exact object kind did not match the authorized kind",
        ));
    }
    let byte_len = u64::try_from(standard.EndOfFile).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the filesystem reported a negative object length",
        )
    })?;

    let attributes_after: FILE_ATTRIBUTE_TAG_INFO =
        query_file_info(object, FileAttributeTagInfo, QueryKind::AttributeTag)?;
    let standard_after: FILE_STANDARD_INFO =
        query_file_info(object, FileStandardInfo, QueryKind::Standard)?;
    let identity_after = query_identity(object)?;
    if identity_after != identity_before
        || attributes_after.FileAttributes != attributes.FileAttributes
        || attributes_after.ReparseTag != attributes.ReparseTag
        || !same_standard_info(&standard_after, &standard)
    {
        return Err(observation_changed());
    }

    Ok(ObjectInfo {
        identity: identity_before,
        kind,
        byte_len,
        link_count: u64::from(standard.NumberOfLinks),
        readonly: attributes.FileAttributes & FILE_ATTRIBUTE_READONLY != 0,
    })
}

#[derive(Clone, Copy)]
enum QueryKind {
    Identity,
    AttributeTag,
    Standard,
}

fn query_identity(object: &File) -> io::Result<FileIdentity> {
    let info: FILE_ID_INFO = query_file_info(object, FileIdInfo, QueryKind::Identity)?;
    Ok(FileIdentity::new(
        info.VolumeSerialNumber,
        info.FileId.Identifier,
    ))
}

fn query_kind(object: &File) -> io::Result<ObjectKind> {
    let standard: FILE_STANDARD_INFO =
        query_file_info(object, FileStandardInfo, QueryKind::Standard)?;
    Ok(standard_kind(&standard))
}

fn standard_kind(standard: &FILE_STANDARD_INFO) -> ObjectKind {
    if standard.Directory {
        ObjectKind::Directory
    } else {
        ObjectKind::RegularFile
    }
}

fn query_file_info<T: Default>(object: &File, class: i32, query: QueryKind) -> io::Result<T> {
    let mut info = T::default();
    let size = u32::try_from(size_of::<T>()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows file-information structure exceeds the API size limit",
        )
    })?;
    // SAFETY: each call site pairs its information class with the generated
    // repr(C) type identified by `query`. The buffer is initialized, writable,
    // correctly sized, and lives through the synchronous call.
    let result = unsafe {
        GetFileInformationByHandleEx(
            raw_handle(object),
            class,
            ptr::from_mut(&mut info).cast::<c_void>(),
            size,
        )
    };
    if result == 0 {
        Err(map_query_error(io::Error::last_os_error(), query))
    } else {
        Ok(info)
    }
}

fn is_remote(object: &File) -> io::Result<bool> {
    let mut info = FILE_REMOTE_PROTOCOL_INFO::default();
    // SAFETY: `info` is the generated repr(C) buffer for
    // FileRemoteProtocolInfo and remains writable for the synchronous call.
    let result = unsafe {
        GetFileInformationByHandleEx(
            raw_handle(object),
            FileRemoteProtocolInfo,
            ptr::from_mut(&mut info).cast::<c_void>(),
            size_of::<FILE_REMOTE_PROTOCOL_INFO>() as u32,
        )
    };
    if result != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error().map(|code| code as u32) {
        // This information class is documented to fail with
        // ERROR_INVALID_PARAMETER for a local handle. No other error proves
        // locality.
        Some(ERROR_INVALID_PARAMETER) => Ok(false),
        _ => Err(map_remote_query_error(error)),
    }
}

fn rename_by_handle(
    source: &File,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    flags: u32,
) -> io::Result<()> {
    let buffer = RenameBuffer::new(target_parent, target_name, flags)?;
    // SAFETY: RenameBuffer owns an aligned initialized FILE_RENAME_INFO and
    // passes the ABI-required `size_of::<FILE_RENAME_INFO>() + FileNameLength`
    // byte count. Both exact handles stay live for the synchronous call.
    let result = unsafe {
        SetFileInformationByHandle(
            raw_handle(source),
            FileRenameInfoEx,
            buffer.as_ptr(),
            buffer.passed_len,
        )
    };
    if result == 0 {
        Err(map_rename_error(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

const _: () = {
    assert!(align_of::<u64>() >= align_of::<FILE_RENAME_INFO>());
    assert!(
        size_of::<FILE_RENAME_INFO>() >= offset_of!(FILE_RENAME_INFO, FileName) + size_of::<u16>()
    );
    assert!(offset_of!(FILE_RENAME_INFO, FileName).is_multiple_of(align_of::<u16>()));
};

struct RenameBuffer {
    storage: Vec<u64>,
    passed_len: u32,
}

impl RenameBuffer {
    fn new(
        target_parent: &DirectoryCapability,
        target_name: &OsStr,
        flags: u32,
    ) -> io::Result<Self> {
        let encoded = target_name.encode_wide().collect::<Vec<_>>();
        let name_bytes = encoded
            .len()
            .checked_mul(size_of::<u16>())
            .ok_or_else(rename_name_too_long)?;
        let name_bytes_u32 = u32::try_from(name_bytes).map_err(|_| rename_name_too_long())?;
        // FILE_RENAME_INFO is the Win32 spelling of the variable-length
        // FILE_RENAME_INFORMATION ABI. SetFileInformationByHandle requires the
        // complete fixed structure size plus FileNameLength bytes.
        let passed_len = size_of::<FILE_RENAME_INFO>()
            .checked_add(name_bytes)
            .ok_or_else(rename_name_too_long)?;
        let passed_len_u32 = u32::try_from(passed_len).map_err(|_| rename_name_too_long())?;
        let word_count = passed_len.div_ceil(size_of::<u64>());
        let mut storage = vec![0_u64; word_count];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        let file_name_offset = offset_of!(FILE_RENAME_INFO, FileName);

        // SAFETY: Vec<u64> supplies sufficient alignment on supported Windows
        // targets. `passed_len` covers the fixed generated structure plus all
        // UTF-16 name bytes, and the destination uses the generated field
        // offset rather than a hand-maintained layout constant.
        unsafe {
            (*info).Anonymous.Flags = flags;
            (*info).RootDirectory = raw_handle(&target_parent.0.file);
            (*info).FileNameLength = name_bytes_u32;
            ptr::copy_nonoverlapping(
                encoded.as_ptr(),
                info.cast::<u8>().add(file_name_offset).cast::<u16>(),
                encoded.len(),
            );
        }

        Ok(Self {
            storage,
            passed_len: passed_len_u32,
        })
    }

    fn as_ptr(&self) -> *const c_void {
        self.storage.as_ptr().cast::<c_void>()
    }

    #[cfg(test)]
    fn file_name_length(&self) -> u32 {
        let info = self.storage.as_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `storage` was initialized by `new` as FILE_RENAME_INFO and
        // remains aligned and live for this read.
        unsafe { (*info).FileNameLength }
    }

    #[cfg(test)]
    fn encoded_name(&self) -> Vec<u16> {
        let info = self.storage.as_ptr().cast::<FILE_RENAME_INFO>();
        let length = self.file_name_length() as usize / size_of::<u16>();
        // SAFETY: `new` allocated and initialized exactly FileNameLength bytes
        // at the generated flexible-array offset.
        unsafe {
            std::slice::from_raw_parts(
                info.cast::<u8>()
                    .add(offset_of!(FILE_RENAME_INFO, FileName))
                    .cast::<u16>(),
                length,
            )
            .to_vec()
        }
    }
}

fn require_mutation_object(object: &ObjectCapability, operation: &str) -> io::Result<()> {
    if object.access() != CapabilityAccess::Mutation {
        return Err(access_required(operation));
    }
    Ok(())
}

fn require_mutation_parent(parent: &DirectoryCapability, operation: &str) -> io::Result<()> {
    if parent.access() != CapabilityAccess::Mutation {
        return Err(access_required(operation));
    }
    Ok(())
}

fn require_shared_qualification(
    left: &Arc<VolumeQualification>,
    right: &Arc<VolumeQualification>,
) -> io::Result<()> {
    if !Arc::ptr_eq(left, right) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem capabilities came from different qualified transaction roots",
        ));
    }
    Ok(())
}

fn require_same_observation(
    observed: io::Result<ObjectInfo>,
    expected_identity: FileIdentity,
    expected_kind: ObjectKind,
) -> io::Result<ObjectInfo> {
    let observed = observed?;
    require_identity(&observed, expected_identity, expected_kind)?;
    Ok(observed)
}

fn require_identity(
    observed: &ObjectInfo,
    expected_identity: FileIdentity,
    expected_kind: ObjectKind,
) -> io::Result<()> {
    if observed.identity != expected_identity || observed.kind != expected_kind {
        return Err(observation_changed());
    }
    Ok(())
}

fn ensure_same_volume(left: FileIdentity, right: FileIdentity) -> io::Result<()> {
    if left.volume_serial_number != right.volume_serial_number {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "exact Windows rename cannot cross volumes",
        ));
    }
    Ok(())
}

fn ensure_not_alias(left: FileIdentity, right: FileIdentity, operation: &str) -> io::Result<()> {
    if left == right {
        return Err(alias_rejected(operation));
    }
    Ok(())
}

fn access_mask(access: CapabilityAccess) -> u32 {
    match access {
        CapabilityAccess::Inspect => FILE_GENERIC_READ | SYNCHRONIZE,
        CapabilityAccess::Mutation => FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE,
    }
}

fn raw_handle(file: &File) -> HANDLE {
    file.as_raw_handle().cast()
}

fn observation_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "filesystem identity or exact observation changed during validation",
    )
}

fn rename_name_too_long() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "the direct child name exceeds the Windows rename API size limit",
    )
}

fn ntstatus_to_io(status: i32) -> io::Error {
    // SAFETY: RtlNtStatusToDosError accepts every NTSTATUS and returns a Win32
    // error code without borrowing external memory.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

fn map_query_error(error: io::Error, query: QueryKind) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_INVALID_PARAMETER) => match query {
            QueryKind::Identity | QueryKind::AttributeTag | QueryKind::Standard => {
                io::Error::new(io::ErrorKind::Unsupported, error)
            }
        },
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_open_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_reopen_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_create_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) => {
            io::Error::new(io::ErrorKind::AlreadyExists, error)
        }
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_rename_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) => {
            io::Error::new(io::ErrorKind::AlreadyExists, error)
        }
        Some(ERROR_NOT_SAME_DEVICE) => io::Error::new(io::ErrorKind::CrossesDevices, error),
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_disposition_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_flush_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_volume_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn map_remote_query_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_CALL_NOT_IMPLEMENTED | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

fn same_standard_info(left: &FILE_STANDARD_INFO, right: &FILE_STANDARD_INFO) -> bool {
    left.AllocationSize == right.AllocationSize
        && left.EndOfFile == right.EndOfFile
        && left.NumberOfLinks == right.NumberOfLinks
        && left.DeletePending == right.DeletePending
        && left.Directory == right.Directory
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use std::{
        fs::{self, OpenOptions as StdOpenOptions},
        os::windows::{ffi::OsStringExt as _, fs::OpenOptionsExt as _},
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn current_directory_file() -> File {
        StdOpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(".")
            .expect("open current directory")
    }

    fn unique_test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "leptos-ui-kit-platform-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn adopt_path(path: &std::path::Path) -> DirectoryCapability {
        let file = StdOpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .expect("open test root");
        let adopted = adopt_root_directory(file).expect("adopt test root");
        probe_volume(adopted)
            .expect("qualify test volume")
            .into_root()
    }

    fn current_qualified_directory() -> DirectoryCapability {
        let adopted =
            adopt_root_directory(current_directory_file()).expect("adopt current directory");
        probe_volume(adopted)
            .expect("qualify current volume")
            .into_root()
    }

    fn create_directory(parent: &DirectoryCapability, name: &str) -> DirectoryCapability {
        let mutation =
            create_directory_nofollow(parent, OsStr::new(name)).expect("create directory");
        sync_mutation_parents(mutation, &[parent])
            .expect("sync directory creation parent")
            .into_inner()
    }

    fn delete_object(parent: &DirectoryCapability, object: ObjectCapability) {
        let mutation = delete_exact(parent, object).expect("delete exact object");
        let () = sync_mutation_parents(mutation, &[parent])
            .expect("sync exact deletion parent")
            .into_inner();
    }

    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "this helper is compiled and executed only on Windows"
    )]
    fn clear_readonly(path: &std::path::Path) {
        let mut permissions = fs::metadata(path).expect("readonly metadata").permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).expect("clear readonly attribute");
    }

    #[test]
    fn full_identity_is_stable_across_repeated_queries() {
        let directory = current_qualified_directory();
        let first = refresh_verified(&directory.0).expect("first observation");
        let second = refresh_verified(&directory.0).expect("second observation");
        assert_eq!(first.identity, second.identity);
        assert!(first.identity.file_id.iter().any(|byte| *byte != 0));
    }

    #[test]
    fn rename_buffer_uses_fixed_structure_plus_full_name_length() {
        let directory = current_qualified_directory();
        let name = OsStr::new("rename-layout-λ");
        let encoded = name.encode_wide().collect::<Vec<_>>();
        let buffer = RenameBuffer::new(&directory, name, 0).expect("rename buffer");
        assert_eq!(
            buffer.passed_len as usize,
            size_of::<FILE_RENAME_INFO>() + encoded.len() * size_of::<u16>()
        );
        assert_eq!(
            buffer.file_name_length() as usize,
            encoded.len() * size_of::<u16>()
        );
        assert_eq!(buffer.encoded_name(), encoded);
    }

    #[test]
    fn direct_name_validation_rejects_unpaired_utf16() {
        let name = OsString::from_wide(&[0xd800]);
        assert!(validate_direct_name(&name).is_err());
    }

    #[test]
    fn invalid_parameter_is_not_reclassified_for_mutation_abis() {
        let mutation_mappers: [fn(io::Error) -> io::Error; 5] = [
            map_reopen_error,
            map_create_error,
            map_rename_error,
            map_disposition_error,
            map_flush_error,
        ];
        for mapper in mutation_mappers {
            let mapped = mapper(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32));
            assert_eq!(mapped.raw_os_error(), Some(ERROR_INVALID_PARAMETER as i32));
        }

        let query = map_query_error(
            io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32),
            QueryKind::Identity,
        );
        assert_eq!(query.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn native_create_move_and_delete_directory_round_trip() {
        let root_path = unique_test_root("move");
        fs::create_dir(&root_path).expect("create test root");
        let root = adopt_path(&root_path);
        let source_parent = create_directory(&root, "source");
        let destination_parent = create_directory(&root, "destination");
        let child = create_directory(&source_parent, "child").into_object();
        let policy = MovePolicy::new(child.info());
        let moved = move_noreplace(
            child,
            &source_parent,
            &destination_parent,
            OsStr::new("moved"),
            policy,
        )
        .expect("move directory no-replace");
        let child = sync_mutation_parents(moved, &[&source_parent, &destination_parent])
            .expect("sync move parents")
            .into_inner();
        delete_object(&destination_parent, child);
        delete_object(&root, source_parent.into_object());
        delete_object(&root, destination_parent.into_object());
        drop(root);
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_case_variant_creation_collides_with_existing_direct_child() {
        let root_path = unique_test_root("case-collision");
        fs::create_dir(&root_path).expect("create test root");
        let root = adopt_path(&root_path);
        let existing = create_directory(&root, "CaseOwner");
        let existing_identity = existing.identity();

        let failure = create_directory_nofollow(&root, OsStr::new("caseowner"))
            .expect_err("case-variant creation must collide");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::AlreadyExists);
        let rebound = open_child_nofollow(
            &root,
            OsStr::new("caseowner"),
            ObjectKind::Directory,
            CapabilityAccess::Inspect,
        )
        .expect("case-insensitive lookup must find the existing child");
        assert_eq!(rebound.identity(), existing_identity);
        drop(rebound);

        delete_object(&root, existing.into_object());
        drop(root);
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_parent_sync_rejects_a_changed_present_postcondition() {
        let root_path = unique_test_root("durability-postcondition");
        fs::create_dir(&root_path).expect("create test root");
        let root = adopt_path(&root_path);
        let mutation = create_directory_nofollow(&root, OsStr::new("created"))
            .expect("create pending directory");
        fs::rename(root_path.join("created"), root_path.join("moved-aside"))
            .expect("move exact created directory aside");
        fs::create_dir(root_path.join("created")).expect("create substitute directory");

        let failure = sync_mutation_parents(mutation, &[&root])
            .expect_err("a substituted binding must not become durable");
        assert_eq!(failure.error().kind(), io::ErrorKind::WouldBlock);
        drop(failure);
        drop(root);
        fs::remove_dir(root_path.join("created")).expect("remove substitute");
        fs::remove_dir(root_path.join("moved-aside")).expect("remove exact created directory");
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_mutators_reject_capabilities_from_another_qualification_lineage() {
        let root_path = unique_test_root("qualification-lineage");
        fs::create_dir(&root_path).expect("create test root");
        let first_root = adopt_path(&root_path);
        let second_root = adopt_path(&root_path);
        let child = create_directory(&first_root, "child").into_object();
        let policy = MovePolicy::new(child.info());

        let failure = move_noreplace(
            child,
            &first_root,
            &second_root,
            OsStr::new("moved"),
            policy,
        )
        .expect_err("separate qualification lineages must not mix");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::InvalidInput);
        let MutationFailure::NotMutated { capabilities, .. } = failure else {
            unreachable!("phase checked above")
        };
        delete_object(&first_root, *capabilities);

        drop(second_root);
        drop(first_root);
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_move_rejects_a_fresh_regular_file_hard_link() {
        let root_path = unique_test_root("move-link-policy");
        fs::create_dir(&root_path).expect("create test root");
        let root = adopt_path(&root_path);
        let source_parent = create_directory(&root, "source");
        let target_parent = create_directory(&root, "target");
        fs::write(root_path.join("source/owner"), b"owner").expect("write owner");
        let owner = open_child_nofollow(
            &source_parent,
            OsStr::new("owner"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open owner");
        let policy = MovePolicy::new(owner.info());
        fs::hard_link(
            root_path.join("source/owner"),
            root_path.join("source/owner-alias"),
        )
        .expect("create owner alias");

        let failure = move_noreplace(
            owner,
            &source_parent,
            &target_parent,
            OsStr::new("placed"),
            policy,
        )
        .expect_err("a fresh hard link must invalidate the move policy");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::WouldBlock);
        drop(failure);

        fs::remove_file(root_path.join("source/owner-alias")).expect("remove owner alias");
        fs::remove_file(root_path.join("source/owner")).expect("remove owner");
        delete_object(&root, source_parent.into_object());
        delete_object(&root, target_parent.into_object());
        drop(root);
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_cross_parent_replacement_syncs_both_parents() {
        let root_path = unique_test_root("cross-parent-replace");
        fs::create_dir(&root_path).expect("create test root");
        let root = adopt_path(&root_path);
        let source_parent = create_directory(&root, "source");
        let target_parent = create_directory(&root, "target");
        fs::write(root_path.join("source/stage"), b"staged bytes").expect("write stage");
        fs::write(root_path.join("target/output"), b"old bytes").expect("write target");
        let source = open_child_nofollow(
            &source_parent,
            OsStr::new("stage"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open stage");
        let target = open_child_nofollow(
            &target_parent,
            OsStr::new("output"),
            ObjectKind::RegularFile,
            CapabilityAccess::Inspect,
        )
        .expect("open target");
        let policy = ReplacementPolicy::new(source.info(), target.info());
        let replacement = replace_exact(
            source,
            &source_parent,
            &target_parent,
            OsStr::new("output"),
            target,
            policy,
        )
        .expect("replace across parents");
        assert_eq!(replacement.required_parent_identities().len(), 2);
        let output = sync_mutation_parents(replacement, &[&target_parent, &source_parent])
            .expect("sync both replacement parents")
            .into_inner();
        assert!(!root_path.join("source/stage").exists());
        assert_eq!(
            fs::read(root_path.join("target/output")).expect("read replacement"),
            b"staged bytes"
        );

        delete_object(&target_parent, output);
        delete_object(&root, source_parent.into_object());
        delete_object(&root, target_parent.into_object());
        drop(root);
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_replacement_rejects_hard_link_aliases() {
        let root_path = unique_test_root("alias");
        fs::create_dir(&root_path).expect("create test root");
        fs::write(root_path.join("source"), b"source").expect("write source");
        fs::hard_link(root_path.join("source"), root_path.join("alias"))
            .expect("create hard-link alias");
        let root = adopt_path(&root_path);
        let source = open_child_nofollow(
            &root,
            OsStr::new("source"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open source");
        let alias = open_child_nofollow(
            &root,
            OsStr::new("alias"),
            ObjectKind::RegularFile,
            CapabilityAccess::Inspect,
        )
        .expect("open alias");
        let policy = ReplacementPolicy::new(source.info(), alias.info());
        let failure = replace_exact(source, &root, &root, OsStr::new("alias"), alias, policy)
            .expect_err("alias replacement must fail");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::InvalidInput);
        let MutationFailure::NotMutated { capabilities, .. } = failure else {
            unreachable!("phase checked above")
        };
        drop(capabilities);
        drop(root);
        fs::remove_file(root_path.join("alias")).expect("remove alias");
        fs::remove_file(root_path.join("source")).expect("remove source");
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_exact_file_replacement_preserves_source_identity() {
        let root_path = unique_test_root("replace");
        fs::create_dir(&root_path).expect("create test root");
        fs::write(root_path.join("source"), b"source bytes").expect("write source");
        fs::write(root_path.join("target"), b"target bytes").expect("write target");
        let root = adopt_path(&root_path);
        let source = open_child_nofollow(
            &root,
            OsStr::new("source"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open source");
        let source_identity = source.identity();
        let target = open_child_nofollow(
            &root,
            OsStr::new("target"),
            ObjectKind::RegularFile,
            CapabilityAccess::Inspect,
        )
        .expect("open target");
        let policy = ReplacementPolicy::new(source.info(), target.info());
        let replaced = replace_exact(source, &root, &root, OsStr::new("target"), target, policy)
            .expect("replace target");
        let replaced = sync_mutation_parents(replaced, &[&root])
            .expect("sync replacement parent")
            .into_inner();
        assert_eq!(replaced.identity(), source_identity);
        assert!(!root_path.join("source").exists());
        assert_eq!(
            fs::read(root_path.join("target")).expect("read target"),
            b"source bytes"
        );
        delete_object(&root, replaced);
        drop(root);
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_replacement_rejects_fresh_readonly_transition() {
        let root_path = unique_test_root("replace-readonly");
        fs::create_dir(&root_path).expect("create test root");
        fs::write(root_path.join("source"), b"source").expect("write source");
        fs::write(root_path.join("target"), b"target").expect("write target");
        let root = adopt_path(&root_path);
        let source = open_child_nofollow(
            &root,
            OsStr::new("source"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open source");
        let target = open_child_nofollow(
            &root,
            OsStr::new("target"),
            ObjectKind::RegularFile,
            CapabilityAccess::Inspect,
        )
        .expect("open target");
        let policy = ReplacementPolicy::new(source.info(), target.info());
        let mut permissions = fs::metadata(root_path.join("target"))
            .expect("target metadata")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(root_path.join("target"), permissions).expect("make target readonly");

        let failure = replace_exact(source, &root, &root, OsStr::new("target"), target, policy)
            .expect_err("fresh readonly transition must fail");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::PermissionDenied);
        drop(failure);
        assert_eq!(
            fs::read(root_path.join("source")).expect("read source"),
            b"source"
        );
        assert_eq!(
            fs::read(root_path.join("target")).expect("read target"),
            b"target"
        );

        clear_readonly(&root_path.join("target"));
        drop(root);
        fs::remove_file(root_path.join("source")).expect("remove source");
        fs::remove_file(root_path.join("target")).expect("remove target");
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_replacement_rejects_fresh_link_count_transition() {
        let root_path = unique_test_root("replace-links");
        fs::create_dir(&root_path).expect("create test root");
        fs::write(root_path.join("source"), b"source").expect("write source");
        fs::write(root_path.join("target"), b"target").expect("write target");
        let root = adopt_path(&root_path);
        let source = open_child_nofollow(
            &root,
            OsStr::new("source"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open source");
        let target = open_child_nofollow(
            &root,
            OsStr::new("target"),
            ObjectKind::RegularFile,
            CapabilityAccess::Inspect,
        )
        .expect("open target");
        let policy = ReplacementPolicy::new(source.info(), target.info());
        fs::hard_link(root_path.join("target"), root_path.join("target-alias"))
            .expect("create target alias");

        let failure = replace_exact(source, &root, &root, OsStr::new("target"), target, policy)
            .expect_err("fresh link-count transition must fail");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::WouldBlock);
        drop(failure);
        drop(root);
        fs::remove_file(root_path.join("target-alias")).expect("remove target alias");
        fs::remove_file(root_path.join("target")).expect("remove target");
        fs::remove_file(root_path.join("source")).expect("remove source");
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_delete_rejects_multiply_linked_regular_file() {
        let root_path = unique_test_root("delete-links");
        fs::create_dir(&root_path).expect("create test root");
        fs::write(root_path.join("object"), b"object").expect("write object");
        fs::hard_link(root_path.join("object"), root_path.join("object-alias"))
            .expect("create object alias");
        let root = adopt_path(&root_path);
        let object = open_child_nofollow(
            &root,
            OsStr::new("object"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open object");

        let failure = delete_exact(&root, object)
            .expect_err("multiply linked regular-file deletion must fail");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::InvalidInput);
        drop(failure);
        drop(root);
        fs::remove_file(root_path.join("object-alias")).expect("remove object alias");
        fs::remove_file(root_path.join("object")).expect("remove object");
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_delete_rejects_fresh_readonly_transition() {
        let root_path = unique_test_root("delete-readonly");
        fs::create_dir(&root_path).expect("create test root");
        fs::write(root_path.join("object"), b"object").expect("write object");
        let root = adopt_path(&root_path);
        let object = open_child_nofollow(
            &root,
            OsStr::new("object"),
            ObjectKind::RegularFile,
            CapabilityAccess::Mutation,
        )
        .expect("open object");
        let mut permissions = fs::metadata(root_path.join("object"))
            .expect("object metadata")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(root_path.join("object"), permissions).expect("make object readonly");

        let failure = delete_exact(&root, object).expect_err("fresh readonly transition must fail");
        assert_eq!(failure.phase(), crate::MutationPhase::NotMutated);
        assert_eq!(failure.error().kind(), io::ErrorKind::PermissionDenied);
        drop(failure);
        clear_readonly(&root_path.join("object"));
        drop(root);
        fs::remove_file(root_path.join("object")).expect("remove object");
        fs::remove_dir(&root_path).expect("remove test root");
    }

    #[test]
    fn native_checkout_volume_satisfies_static_probe() {
        let adopted =
            adopt_root_directory(current_directory_file()).expect("adopt current directory");
        let qualified = probe_volume(adopted).expect("probe checkout volume");
        let capabilities = qualified.capabilities();
        assert!(capabilities.supports_hard_links);
        assert!(capabilities.supports_reparse_points);
        assert!(
            capabilities.file_system_name.eq_ignore_ascii_case("NTFS")
                || capabilities.file_system_name.eq_ignore_ascii_case("ReFS")
        );
    }
}
