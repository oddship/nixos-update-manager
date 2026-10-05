// Pure state presentation shared by the Shell extension and its tests.
const states = {
    checking: ['Checking', 'Checking for updates', 'emblem-synchronizing-symbolic'],
    preparing: ['Preparing', 'Preparing your update', 'emblem-synchronizing-symbolic'],
    reviewing: ['Reviewing', 'Refreshing the runtime review', 'emblem-synchronizing-symbolic'],
    updates_found: ['Updates', 'Updates available to prepare', 'software-update-available-symbolic'],
    input_only: ['Updates', 'New input revisions available', 'software-update-available-symbolic'],
    ready: ['Ready', 'Update ready to review and apply', 'software-update-available-symbolic'],
    review_stale: ['Review', 'Running system changed; refresh the review', 'software-update-available-symbolic'],
    stale: ['Check again', 'Configuration changed; check again', 'software-update-available-symbolic'],
    awaiting_authentication: ['Authenticate', 'Waiting for system authentication', 'emblem-synchronizing-symbolic'],
    applying: ['Applying', 'Applying the reviewed system', 'emblem-synchronizing-symbolic'],
    committing: ['Saving', 'Saving the lock update', 'emblem-synchronizing-symbolic'],
    applied: ['Updated', 'System update applied', 'emblem-ok-symbolic'],
    committed: ['Updated', 'System updated and lock change committed', 'emblem-ok-symbolic'],
    up_to_date: ['', 'No updates found for the selected inputs', 'emblem-ok-symbolic'],
    failed: ['Stopped', 'Update stopped; open the app for details', 'software-update-available-symbolic'],
    cancelled: ['', 'Update cancelled', 'software-update-available-symbolic'],
    apply_needs_attention: ['Review result', 'Review the recorded apply result in the app', 'dialog-warning-symbolic'],
    commit_needs_attention: ['Retry commit', 'System updated; the lock commit needs attention', 'software-update-available-symbolic'],
};

export function presentation(state) {
    return states[state] ?? ['', 'Open NixOS Update Manager to check your configuration', 'software-update-available-symbolic'];
}

export function latestFor(records, settings, canonicalize) {
    if (!settings || typeof settings.path !== 'string' || !settings.path.startsWith('/') || typeof settings.host !== 'string')
        return null;
    const matches = records.filter(record => {
        const repo = record?.repository;
        if (record?.schema_version !== 1 || typeof repo?.root !== 'string' || !repo.root.startsWith('/') || typeof repo?.flake_dir !== 'string')
            return false;
        const path = canonicalize(`${repo.root}/${repo.flake_dir}`);
        return path === canonicalize(settings.path) && record.host === settings.host && typeof record.state === 'string';
    });
    const timestamp = record => Number.isFinite(record.created_at_millis) ? record.created_at_millis : (Number.isFinite(record.created_at) ? record.created_at * 1000 : 0);
    return matches.sort((a, b) => timestamp(b) - timestamp(a))[0] ?? null;
}
