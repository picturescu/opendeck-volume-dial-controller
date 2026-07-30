"use strict";

window.createApplicationSelectorPI = function createApplicationSelectorPI(configuration) {
  return function connectElgatoStreamDeckSocket(port, uuid, registerEvent, info, actionInfo) {
    const parsed = typeof actionInfo === "string" ? JSON.parse(actionInfo) : actionInfo;
    const action = parsed.action;
    const context = parsed.context;
    let settings = parsed.payload?.settings ?? {};
    let targets = [];
    let activeTargetId = null;
    let focused = false;
    const sourceIds = Array.isArray(settings.target_ids) && settings.target_ids.length
      ? settings.target_ids
      : (settings.target_id ? [settings.target_id] : []);
    let selectedIds = [...new Set(sourceIds.filter(Boolean))];

    const search = document.getElementById("search");
    const applications = document.getElementById("applications");
    const priority = document.getElementById("priority");
    const status = document.getElementById("status");
    const refresh = document.getElementById("refresh");
    const socket = new WebSocket(`ws://127.0.0.1:${port}`);

    status.textContent = "Connecting…";
    configuration.initialize?.(settings);

    const requestTargets = () => {
      if (socket.readyState !== WebSocket.OPEN) {
        status.textContent = "WebSocket connection failed";
        return;
      }
      status.textContent = "Loading applications…";
      socket.send(JSON.stringify({
        event: "sendToPlugin",
        action,
        context,
        payload: { event: "requestAudioTargets" }
      }));
    };

    const save = () => {
      settings = {
        ...settings,
        target_ids: [...selectedIds],
        target_id: "",
        target_name: "",
        ...(configuration.extraSettings?.() ?? {})
      };
      socket.send(JSON.stringify({ event: "setSettings", context, payload: settings }));
    };

    const nameFor = id => targets.find(item => item.id === id)?.name ||
      id.split("\u001f").pop() || id;

    const render = () => {
      const filter = search.value.trim().toLowerCase();
      applications.innerHTML = "";
      for (const item of targets.filter(item =>
        `${item.name} ${item.detail}`.toLowerCase().includes(filter))) {
        const row = document.createElement("label");
        row.className = "pick-row";
        const box = document.createElement("input");
        box.type = "checkbox";
        box.checked = selectedIds.includes(item.id);
        box.onchange = () => {
          if (box.checked && !selectedIds.includes(item.id)) selectedIds.push(item.id);
          if (!box.checked) selectedIds = selectedIds.filter(id => id !== item.id);
          render();
          save();
        };
        const text = document.createElement("span");
        text.textContent = item.detail ? `${item.name} — ${item.detail}` : item.name;
        row.append(box, text);
        applications.appendChild(row);
      }

      priority.innerHTML = "";
      selectedIds.forEach((id, index) => {
        const available = targets.some(item => item.id === id);
        const row = document.createElement("div");
        row.className = `priority-row${id === activeTargetId ? " active" : ""}`;
        const text = document.createElement("span");
        text.textContent = `${index + 1}. ${nameFor(id)}${id === activeTargetId ? " (Active)" : available ? "" : " (Unavailable)"}`;
        const button = (label, operation) => {
          const element = document.createElement("button");
          element.type = "button";
          element.textContent = label;
          element.onclick = operation;
          return element;
        };
        row.append(
          text,
          button("↑", () => {
            if (index) [selectedIds[index - 1], selectedIds[index]] = [id, selectedIds[index - 1]];
            render(); save();
          }),
          button("↓", () => {
            if (index + 1 < selectedIds.length) [selectedIds[index], selectedIds[index + 1]] = [selectedIds[index + 1], id];
            render(); save();
          }),
          button("Remove", () => {
            selectedIds = selectedIds.filter(value => value !== id);
            render(); save();
          })
        );
        priority.appendChild(row);
      });
    };

    socket.onmessage = event => {
      const message = JSON.parse(event.data);
      console.log(`[${configuration.logName}] received`, message);
      if (message.event !== "sendToPropertyInspector" ||
          message.payload?.event !== "audioTargetList") return;
      targets = Array.isArray(message.payload.targets) ? message.payload.targets : [];
      activeTargetId = message.payload.activeTargetId || null;
      focused = Boolean(message.payload.focused);
      render();
      if (message.payload.error) {
        status.textContent = `Audio enumeration failed: ${message.payload.error}`;
      } else if (!targets.length && selectedIds.length) {
        status.textContent = "Configured applications unavailable";
      } else if (!targets.length) {
        status.textContent = "No active audio applications";
      } else {
        status.textContent = `${focused ? "Focused — " : ""}Found ${targets.length} applications`;
      }
    };
    socket.onerror = event => {
      console.error(`[${configuration.logName}] websocket error`, event);
      status.textContent = "WebSocket connection failed";
    };
    socket.onclose = event => {
      console.warn(`[${configuration.logName}] websocket closed`, event);
      status.textContent = "WebSocket connection failed";
    };
    socket.onopen = () => {
      console.log(`[${configuration.logName}] connected`);
      socket.send(JSON.stringify({ event: registerEvent, uuid }));
      window.setTimeout(requestTargets, 150);
    };

    search.oninput = render;
    refresh.onclick = requestTargets;
    configuration.bindSave?.(save);
    render();
  };
};

const selectorMode = document.documentElement.dataset.selectorMode;
const selectorConfiguration = selectorMode === "dial" ? {
  logName: "volume-dial PI",
  initialize(settings) {
    document.getElementById("custom-title").value = settings.custom_title || "";
    document.getElementById("step").value = settings.volume_step || 2;
    document.getElementById("maximum-volume").value =
      Number(settings.maximum_volume) === 150 ? "150" : "100";
  },
  extraSettings() {
    return {
      custom_title: document.getElementById("custom-title").value,
      volume_step: Number(document.getElementById("step").value),
      maximum_volume: Number(document.getElementById("maximum-volume").value)
    };
  },
  bindSave(save) {
    document.getElementById("custom-title").onchange = save;
    document.getElementById("step").onchange = save;
    document.getElementById("maximum-volume").onchange = save;
  }
} : {
  logName: "app-selector PI",
  initialize(settings) {
    document.getElementById("custom-title").value = settings.custom_title || "";
    document.getElementById("focus-group").value = settings.focus_group || "main";
  },
  extraSettings() {
    return {
      custom_title: document.getElementById("custom-title").value,
      focus_group: document.getElementById("focus-group").value.trim() || "main"
    };
  },
  bindSave(save) {
    document.getElementById("custom-title").onchange = save;
    document.getElementById("focus-group").onchange = save;
  }
};

window.connectElgatoStreamDeckSocket =
  window.createApplicationSelectorPI(selectorConfiguration);
