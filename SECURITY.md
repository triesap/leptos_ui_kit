# Security Policy

Report security issues privately to Tyson Lupul at `tyson@radroots.org`.

Please include the affected crate, version, impact, and a minimal reproduction
when possible. Public issues are welcome after coordinated disclosure.

Installer writes use a project advisory lock, no-follow path checks, durable
journals, and atomic local-filesystem replacement. Symlinks, Windows reparse
points, readonly targets, and unexpected file types fail closed.

Transaction guarantees require ordinary local-filesystem locking, hard-link,
rename, and directory-sync semantics. Network and userspace filesystems that
weaken those primitives are unsupported. File bytes and ordinary POSIX modes
are preserved; ACLs, extended attributes, ownership, and timestamps are not.
