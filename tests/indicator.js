import GLib from 'gi://GLib';
import {latestFor, presentation, needsCheck, recordPresentation, SUCCESS_TTL, CHECK_INTERVAL} from '../extension/model.js';
function assert(value, message) {
    if (!value)
        throw new Error(message);
}
const normalize = path => GLib.canonicalize_filename(path, null);
const settings = {path: '/config/flake/', host: 'desktop'};
const record = (host, created, state = 'ready') => ({schema_version: 1, repository: {root: '/config', flake_dir: 'flake'}, host, created_at_millis: created, state});
assert(latestFor([record('desktop', 1), record('other', 99)], settings, normalize).host === 'desktop', 'Filter by host before selecting latest');
assert(latestFor([record('desktop', 1), record('desktop', 2, 'preparing')], settings, normalize).state === 'preparing', 'Show the latest matching operation');
assert(latestFor([{...record('desktop', 3), repository: {root: '/elsewhere', flake_dir: ''}}], settings, normalize) === null, 'Do not leak another folder’s state');
assert(latestFor([{...record('desktop', 3), repository: {root: '/config', flake_dir: 'flake/hosts/desktop'}}], settings, normalize)?.created_at_millis === 3, 'Match per-host flakes under the chosen folder');
assert(latestFor([{...record('desktop', 3), repository: {root: '/config', flake_dir: 'flake/hosts/other'}}], settings, normalize) === null, 'Do not match another host’s flake');
assert(latestFor([record('desktop', 3)], null, normalize) === null, 'Missing settings is neutral');
assert(latestFor([{schema_version: 1}], settings, normalize) === null, 'Malformed records are ignored');
assert(latestFor([{...record('desktop', 3), schema_version: 99}], settings, normalize) === null, 'Unknown schemas are ignored');
assert(presentation('apply_needs_attention')[1].includes('apply result'), 'An uncertain apply must never look successful');
assert(presentation('commit_needs_attention')[0] === 'Retry commit', 'Commit failure must distinguish the already applied system');
assert(presentation('preparing')[0] === 'Preparing', 'Expose active progress');
assert(presentation('future_state')[0] === '', 'Unknown states must not invent a result');

const completed = {...record('desktop', 1, 'committed'), modified_at_millis: 1000};
assert(!needsCheck(completed, 1000 + SUCCESS_TTL - 1), 'Keep recent success visible');
assert(needsCheck(completed, 1000 + SUCCESS_TTL), 'Expire success at its TTL');
assert(recordPresentation(completed, 1000 + SUCCESS_TTL)[0] === '', 'Expired success must not say Updated');
assert(recordPresentation(completed, 1000)[0] === 'Updated', 'Use modification time after a long operation');
assert(needsCheck({...completed, state: 'applied'}, 1000 + SUCCESS_TTL), 'Applied success also expires');
assert(!needsCheck({...completed, state: 'up_to_date'}, 1000 + CHECK_INTERVAL - 1), 'Avoid excessive availability checks');
assert(needsCheck({...completed, state: 'up_to_date'}, 1000 + CHECK_INTERVAL), 'Recheck availability periodically');
for (const state of ['checking', 'preparing', 'ready', 'review_stale', 'applying', 'commit_needs_attention', 'apply_needs_attention'])
    assert(!needsCheck({...completed, state}, 1000 + CHECK_INTERVAL * 10), `Preserve ${state}`);
assert(needsCheck(null), 'Check configured hosts without history');
assert(needsCheck({...completed, modified_at_millis: undefined, created_at_millis: undefined}), 'Legacy records expire');
assert(needsCheck({...completed, state: 'updates_found'}, 1000 + CHECK_INTERVAL), 'Refresh available input revisions too');
print('Indicator selection, expiry, and availability checks passed.');
