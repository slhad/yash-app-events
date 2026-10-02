// SPEC-OBS-004. Also loaded by the Node tests; no Qt or browser dependencies.
var pollMs = 2000;
var timeoutMs = 5000;
var freshMs = 5000;
var maximumMessage = 1024 * 1024;

function displayText(value, maximum) {
    return String(value === null || value === undefined ? "" : value)
        .replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, " ")
        .slice(0, maximum || 240);
}

function rate(value) {
    return typeof value === "number" && isFinite(value) && value >= 0
        ? value.toFixed(1) : "?";
}

function presentation(status, profileName, elapsed, connectionError) {
    var label = "Offline";
    var symbol = "○";
    var alarming = false;
    var reason = connectionError || "Daemon unavailable";
    if (status) {
        reason = "";
        if (status.capture_error || status.output_error) {
            label = "Error";
            symbol = "!";
            alarming = true;
            reason = status.capture_error || status.output_error;
        } else if (!status.capture_active) {
            label = status.active_profile ? "Stopped" : "Idle";
        } else if (!status.active_profile) {
            label = "Capturing";
            symbol = "◉";
            reason = "Capture active; no profile selected";
        } else if (status.last_analysis_age_ms === undefined) {
            label = "Capturing";
            symbol = "◉";
            reason = "Daemon does not report analysis freshness; update the daemon";
        } else if (status.last_analysis_age_ms === null) {
            label = "Waiting";
            symbol = "◌";
            reason = "Waiting for the first completed profile analysis";
        } else if (typeof status.last_analysis_age_ms !== "number"
                   || !isFinite(status.last_analysis_age_ms)
                   || status.last_analysis_age_ms < 0
                   || status.last_analysis_age_ms + elapsed > freshMs) {
            label = "Stalled";
            symbol = "!";
            alarming = true;
            reason = "No completed profile analysis in the last 5 seconds";
        } else {
            label = "Processing";
            symbol = "●";
        }
    }
    var profile = status && status.capture_active && status.active_profile
        ? displayText(profileName || status.active_profile, 80) : "None";
    var lines = ["Yash Events: " + label];
    if (status) {
        if (status.capture_active && status.active_profile) lines.push("Profile: " + profile);
        lines.push("Source: " + displayText(status.selected_source || "None"));
        lines.push("Capture: " + (status.capture_active ? "active" : "stopped"));
        lines.push("Input / analysis: " + rate(status.input_fps) + " / "
                   + rate(status.analysis_fps) + " FPS (session averages)");
        if (typeof status.last_analysis_age_ms === "number")
            lines.push("Last analysis: " + ((status.last_analysis_age_ms + elapsed) / 1000).toFixed(1) + " s ago");
        lines.push("Replaced frames: " + displayText(status.replaced_frames));
        lines.push("Detector errors this session: " + displayText(status.detector_errors));
        if (status.capture_error) lines.push("Capture error: " + displayText(status.capture_error));
        if (status.output_error) lines.push("Output error: " + displayText(status.output_error));
    }
    if (reason) lines.push(displayText(reason));
    lines.push("Left click: open editor · Right click: refresh profile name");
    return {label: label, symbol: symbol, alarming: alarming,
            profile: profile, tooltip: lines.join("\n")};
}

function createClient(hooks) {
    var client = {
        ready: false, pending: null, status: null, sampledAt: 0,
        profileId: null, profileName: "", connectionError: "", nextId: 0
    };
    function notify() { hooks.changed(); }
    function request(method, params, now) {
        if (client.pending) return;
        client.pending = {id: ++client.nextId, method: method, sentAt: now};
        hooks.send(JSON.stringify({jsonrpc: "2.0", id: client.pending.id,
                                  method: method, params: params || {}}) + "\n");
    }
    client.disconnected = function(reason) {
        client.ready = false;
        client.pending = null;
        client.status = null;
        client.profileId = null;
        client.profileName = "";
        client.connectionError = reason || client.connectionError || "Daemon disconnected";
        notify();
    };
    function fail(reason) {
        client.disconnected(reason);
        hooks.disconnect();
    }
    client.connected = function(now) {
        client.disconnected("Connecting to daemon");
        request("system.handshake", {protocol: 1, client_name: "yash-quickshell",
                                    client_version: "0.1.1"}, now);
    };
    client.poll = function(now) {
        if (client.ready && !client.pending) request("system.status", {}, now);
    };
    client.refresh = function(now) {
        client.profileId = null;
        client.profileName = "";
        client.poll(now);
    };
    client.tick = function(now) {
        if (client.pending && now - client.pending.sentAt >= timeoutMs)
            fail("Daemon response timed out");
        notify();
    };
    client.receive = function(line, now) {
        if (line.length > maximumMessage) return fail("Daemon reply exceeds 1 MiB");
        var reply;
        try { reply = JSON.parse(line); }
        catch (error) { return fail("Malformed daemon reply"); }
        if (!reply || reply.jsonrpc !== "2.0" || !client.pending
            || reply.id !== client.pending.id
            || ((reply.result === undefined) === (reply.error === undefined)))
            return fail("Unexpected daemon reply");
        var method = client.pending.method;
        client.pending = null;
        if (reply.error) {
            // A deleted/unreadable profile must not hide otherwise useful status.
            if (method === "profile.get") return notify();
            return fail("Daemon rejected " + method + ": " + displayText(reply.error.message));
        }
        var result = reply.result;
        if (method === "system.handshake") {
            if (!result || result.protocol !== 1) return fail("Incompatible daemon protocol");
            client.ready = true;
            client.connectionError = "";
            client.poll(now);
        } else if (method === "system.status") {
            if (!result || typeof result.capture_active !== "boolean"
                || (result.active_profile !== null && typeof result.active_profile !== "string"))
                return fail("Invalid daemon status");
            client.status = result;
            client.sampledAt = now;
            var visibleProfileId = result.capture_active ? result.active_profile : null;
            if (visibleProfileId !== client.profileId) {
                client.profileId = visibleProfileId;
                client.profileName = "";
                if (client.profileId) request("profile.get", {profile_id: client.profileId}, now);
            }
        } else if (method === "profile.get") {
            if (result && result.id === client.profileId && typeof result.name === "string")
                client.profileName = displayText(result.name, 80);
        }
        notify();
    };
    client.view = function(now) {
        return presentation(client.status, client.profileName,
                            Math.max(0, now - client.sampledAt), client.connectionError);
    };
    return client;
}
