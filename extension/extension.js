import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {latestFor, presentation} from './model.js';

const APP = 'io.github.oddship.NixOSUpdates.desktop';
const LIMIT = 1024 * 1024;
const RECORD_LIMIT = 256;

// Never run Nix, Git, or privileged operations in the Shell process.
export default class NixOSUpdatesIndicator extends Extension {
    enable() {
        this._cancel = new Gio.Cancellable();
        this._state = Gio.File.new_for_path(GLib.build_filenamev([GLib.get_user_state_dir(), 'nixos-updates']));
        this._button = new PanelMenu.Button(0.0, 'NixOS Update Manager');
        const box = new St.BoxLayout();
        this._icon = new St.Icon({icon_name: 'software-update-available-symbolic', style_class: 'system-status-icon'});
        this._label = new St.Label({text: '', y_align: Clutter.ActorAlign.CENTER, style_class: 'nixos-updates-label'});
        box.add_child(this._icon);
        box.add_child(this._label);
        this._button.add_child(box);
        this._status = new PopupMenu.PopupMenuItem('Open NixOS Update Manager to check your configuration', {reactive: false, can_focus: false});
        this._host = new PopupMenu.PopupMenuItem('', {reactive: false, can_focus: false});
        this._open = new PopupMenu.PopupMenuItem('Open NixOS Update Manager');
        this._open.connect('activate', () => {
            const app = Shell.AppSystem.get_default().lookup_app(APP);
            if (app) {
                Main.overview.hide();
                app.activate();
            } else
                this._status.label.text = 'Install NixOS Update Manager to open the review';
        });
        this._button.menu.addMenuItem(this._status);
        this._button.menu.addMenuItem(this._host);
        this._button.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._button.menu.addMenuItem(this._open);
        Main.panel.addToStatusArea(this.uuid, this._button);
        this._queue();
        // Retry when the app has never created its state directory; otherwise monitors drive updates.
        this._fallback = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 15, () => {
            if (!this._rootMonitor || !this._watched)
                this._queue();
            return GLib.SOURCE_CONTINUE;
        });
    }

    disable() {
        this._cancel?.cancel();
        this._cancel = null;
        for (const source of [this._queued, this._fallback]) {
            if (source)
                GLib.source_remove(source);
        }
        this._queued = this._fallback = 0;
        this._rootMonitor?.cancel();
        this._recordMonitor?.cancel();
        this._rootMonitor = this._recordMonitor = null;
        this._watched = null;
        this._button?.destroy();
        this._button = this._icon = this._label = this._status = this._host = this._open = null;
    }

    _queue(delay = 150) {
        if (!this._cancel || this._queued)
            return;
        this._queued = GLib.timeout_add(GLib.PRIORITY_DEFAULT, delay, () => {
            this._queued = 0;
            this._refresh();
            return GLib.SOURCE_REMOVE;
        });
    }

    async _read(file, cancel) {
        const info = await new Promise((resolve, reject) => file.query_info_async(
            'standard::type,standard::size', Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS,
            GLib.PRIORITY_DEFAULT, cancel, (object, result) => {
                try { resolve(object.query_info_finish(result)); } catch (error) { reject(error); }
            }));
        if (info.get_file_type() !== Gio.FileType.REGULAR || info.get_size() > LIMIT)
            throw new Error('Unsupported state file');
        const stream = await new Promise((resolve, reject) => file.read_async(GLib.PRIORITY_DEFAULT, cancel, (object, result) => {
            try { resolve(object.read_finish(result)); } catch (error) { reject(error); }
        }));
        try {
            const bytes = await new Promise((resolve, reject) => stream.read_bytes_async(LIMIT + 1, GLib.PRIORITY_DEFAULT, cancel, (object, result) => {
                try { resolve(object.read_bytes_finish(result)); } catch (error) { reject(error); }
            }));
            if (bytes.get_size() > LIMIT)
                throw new Error('State file too large');
            return JSON.parse(new TextDecoder().decode(bytes.get_data()));
        } finally {
            // A local read stream close is short; use async close to keep Shell responsive.
            stream.close_async(GLib.PRIORITY_DEFAULT, null, (object, result) => {
                try { object.close_finish(result); } catch (_error) { /* Already closed. */ }
            });
        }
    }

    async _records(cancel) {
        const enumerator = await new Promise((resolve, reject) => this._state.enumerate_children_async(
            'standard::name,standard::type', Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS,
            GLib.PRIORITY_DEFAULT, cancel, (object, result) => {
                try { resolve(object.enumerate_children_finish(result)); } catch (error) { reject(error); }
            }));
        const records = [];
        let count = 0;
        try {
            while (true) {
                const batch = await new Promise((resolve, reject) => enumerator.next_files_async(32, GLib.PRIORITY_DEFAULT, cancel, (object, result) => {
                    try { resolve(object.next_files_finish(result)); } catch (error) { reject(error); }
                }));
                if (batch.length === 0)
                    break;
                for (const info of batch) {
                    const name = info.get_name();
                    if (info.get_file_type() !== Gio.FileType.DIRECTORY || !/^candidate-[A-Za-z0-9]+$/.test(name))
                        continue;
                    if (++count > RECORD_LIMIT)
                        throw new Error('Open app for larger histories');
                    const record = await this._read(this._state.get_child(name).get_child('state.json'), cancel);
                    if (record?.id === name)
                        records.push(record);
                }
            }
        } finally {
            enumerator.close_async(GLib.PRIORITY_DEFAULT, null, (object, result) => {
                try { object.close_finish(result); } catch (_error) { /* Already closed. */ }
            });
        }
        return records;
    }

    _show(record, settings) {
        const [label, status, icon] = presentation(record?.state);
        this._label.text = label;
        this._label.visible = label.length > 0;
        this._icon.icon_name = icon;
        this._button.accessible_name = `NixOS Update Manager: ${status}`;
        this._status.label.text = status;
        this._host.label.text = settings?.host ? `Host: ${settings.host.slice(0, 64)}` : '';
        this._host.visible = Boolean(settings?.host);
        this._open.label.text = record?.state === 'ready' ? 'Review update' : 'Open NixOS Update Manager';
    }

    async _refresh() {
        if (this._refreshing) {
            this._again = true;
            return;
        }
        const cancel = this._cancel;
        if (!cancel)
            return;
        this._refreshing = true;
        try {
            if (!this._rootMonitor) {
                this._rootMonitor = this._state.monitor_directory(Gio.FileMonitorFlags.NONE, cancel);
                this._rootMonitor.connect('changed', (_monitor, file) => {
                    const name = file?.get_basename() ?? '';
                    if (name === 'settings.json' || name.startsWith('candidate-'))
                        this._queue();
                });
            }
            const settings = await this._read(this._state.get_child('settings.json'), cancel).catch(() => null);
            const records = settings ? await this._records(cancel) : [];
            if (cancel.is_cancelled() || cancel !== this._cancel)
                return;
            const record = latestFor(records, settings, path => GLib.canonicalize_filename(path, null));
            this._show(record, settings);
            const watched = record?.id ?? null;
            if (watched !== this._watched) {
                this._recordMonitor?.cancel();
                this._recordMonitor = null;
                this._watched = watched;
                if (watched) {
                    this._recordMonitor = this._state.get_child(watched).monitor_directory(Gio.FileMonitorFlags.NONE, cancel);
                    this._recordMonitor.connect('changed', (_monitor, file) => {
                        if (file?.get_basename() === 'state.json')
                            this._queue();
                    });
                    // Catch a transition between the first read and monitor installation.
                    this._queue();
                }
            }
        } catch (_error) {
            if (!cancel.is_cancelled() && cancel === this._cancel) {
                this._show(null, null);
                this._status.label.text = 'Open NixOS Update Manager to view current status';
                this._queue(15000);
            }
        } finally {
            this._refreshing = false;
            if (this._again) {
                this._again = false;
                this._queue();
            }
        }
    }
}
