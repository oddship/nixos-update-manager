import GLib from 'gi://GLib';
import {latestFor, presentation} from '../extension/model.js';
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
assert(latestFor([record('desktop', 3)], null, normalize) === null, 'Missing settings is neutral');
assert(latestFor([{schema_version: 1}], settings, normalize) === null, 'Malformed records are ignored');
assert(latestFor([{...record('desktop', 3), schema_version: 99}], settings, normalize) === null, 'Unknown schemas are ignored');
assert(presentation('apply_needs_attention')[1].includes('apply result'), 'An uncertain apply must never look successful');
assert(presentation('commit_needs_attention')[0] === 'Retry commit', 'Commit failure must distinguish the already applied system');
assert(presentation('preparing')[0] === 'Preparing', 'Expose active progress');
assert(presentation('future_state')[0] === '', 'Unknown states must not invent a result');
print('Indicator selection and status checks passed.');
