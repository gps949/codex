"use strict";

(() => {
  const { t } = window.AccountManagerRoutingMessages;
  let currentView,
    currentApi,
    renderedKey,
    renderedVersion,
    renderedLanguage,
    collectDraft,
    dirty = false;
  let saving = false;
  let taskDraft = "";
  let restoredDraft = null;

  function configuration(view) {
    return {
      mode: "off",
      source: "local",
      main_tasks: true,
      subagents: true,
      preference: 50,
      max_effort: "high",
      send_task_description: false,
      local_fallback: false,
      allowed_models: [],
      model_roles: {},
      ...view?.config,
    };
  }

  function roleLabel(role) {
    return t(
      { economy: "Economy", balanced: "Balanced", capability: "Capability" }[
        role
      ] || "Unassigned",
    );
  }

  function render(view, api) {
    currentView = view;
    currentApi = api;
    const root = document.getElementById("model-routing");
    if (!root) return;
    root.hidden = !view;
    if (!view) return;
    const language = window.AccountManagerMessages.language();
    const key = `${view.userConfigVersion}:${language}:${JSON.stringify(view.models)}:${JSON.stringify(view.lastDecision)}:${view.decisionServiceReady}:${JSON.stringify(view.effectiveConfig)}`;
    if (
      renderedKey &&
      (saving ||
        ((dirty || root.contains(document.activeElement)) &&
          language === renderedLanguage) ||
        key === renderedKey)
    ) {
      for (const input of root.querySelectorAll(
        "input, select, button, textarea",
      ))
        input.disabled = api.busy || saving;
      const conflict = root.querySelector(".routing-conflict");
      if (conflict)
        conflict.hidden = view.userConfigVersion === renderedVersion || saving;
      return;
    }
    const draft = restoredDraft || (dirty ? collectDraft() : null);
    const version =
      restoredDraft?.version ||
      (dirty ? renderedVersion : view.userConfigVersion);
    restoredDraft = null;
    renderedKey = key;
    renderedVersion = version;
    renderedLanguage = language;
    const saved = draft?.config || configuration(view);
    root.replaceChildren();
    const el = api.element;
    const title = el("h2", t("Automatic model selection"));
    title.id = "model-routing-title";
    root.setAttribute("aria-labelledby", title.id);
    root.append(
      title,
      el(
        "p",
        t(
          "Manual model choices take priority. Active tasks keep their admitted model.",
        ),
        "muted",
      ),
    );
    if (view.overridden) {
      root.append(
        el(
          "p",
          t("Higher-priority settings override this saved policy."),
          "message warning",
        ),
      );
      const policy = configuration(view);
      const effective = configuration({ config: view.effectiveConfig });
      const enabled = (value) => t(value ? "On" : "Off");
      const enumLabel = (value) =>
        t(
          {
            off: "Off",
            preview: "Preview only",
            automatic: "Automatic",
            local: "Local rules",
            decision_service: "Configured Jev / Clef",
          }[value] || value,
        );
      const differences = el(
        "details",
        "",
        "control-disclosure routing-overrides",
      );
      differences.append(el("summary", t("Saved → Effective")));
      const list = el("ul");
      for (const [name, label, format = String] of [
        ["mode", "Selection mode", enumLabel],
        ["source", "Decision source", enumLabel],
        ["main_tasks", "New main tasks", enabled],
        ["subagents", "New subagents", enabled],
        ["preference", "Preference"],
        ["max_effort", "Highest automatic effort"],
        [
          "send_task_description",
          "Send a short task description to the configured service",
          enabled,
        ],
        ["local_fallback", "Use local rules if the service fails", enabled],
        [
          "allowed_models",
          "Available models",
          (value) =>
            value.length
              ? [...value].sort().join(", ")
              : t("All verified models"),
        ],
        [
          "model_roles",
          "Model roles",
          (value) =>
            Object.keys(value)
              .sort()
              .map((model) => `${model}: ${roleLabel(value[model])}`)
              .join(", ") || t("Use catalog roles"),
        ],
      ]) {
        const savedValue = format(policy[name]);
        const effectiveValue = format(effective[name]);
        if (savedValue !== effectiveValue)
          list.append(
            el(
              "li",
              t("{setting}: {saved} → {effective}", {
                setting: t(label),
                saved: savedValue,
                effective: effectiveValue,
              }),
            ),
          );
      }
      differences.append(list);
      root.append(differences);
    }
    const form = el("form", "", "routing-form");
    const grid = el("div", "", "settings-grid");
    form.append(grid);
    function field(
      parent,
      name,
      label,
      type,
      value,
      help = "",
      attributes = {},
    ) {
      return api.field(
        parent,
        `routing-${name}`,
        t(label),
        type,
        value,
        t(help),
        attributes,
      );
    }
    const mode = field(
      grid,
      "mode",
      "Selection mode",
      "select",
      saved.mode,
      "",
      {
        options: [
          ["off", t("Off")],
          ["preview", t("Preview only")],
          ["automatic", t("Automatic")],
        ],
      },
    );
    const source = field(
      grid,
      "source",
      "Decision source",
      "select",
      saved.source,
      "",
      {
        options: [
          ["local", t("Local rules")],
          ["decision_service", t("Configured Jev / Clef")],
        ],
      },
    );
    const switches = el("div", "", "routing-switches");
    const main = field(
      switches,
      "main",
      "New main tasks",
      "checkbox",
      saved.main_tasks,
    );
    const child = field(
      switches,
      "child",
      "New subagents",
      "checkbox",
      saved.subagents,
    );
    form.append(switches);
    const bias = field(
      form,
      "preference",
      "Preference",
      "range",
      saved.preference,
      "",
      { min: 0, max: 100, step: 1 },
    );
    const output = el("output", String(saved.preference));
    output.htmlFor = bias.id;
    const scale = el("div", "", "routing-scale");
    scale.append(
      el("span", t("Longer use")),
      output,
      el("span", t("More capability")),
    );
    bias.closest(".form-field").append(scale);
    bias.addEventListener("input", () => {
      output.textContent = bias.value;
    });

    const advanced = el("details", "", "control-disclosure");
    advanced.append(el("summary", t("Model limits and service settings")));
    const effort = field(
      advanced,
      "effort",
      "Highest automatic effort",
      "select",
      saved.max_effort,
      "",
      {
        options: ["minimal", "low", "medium", "high", "xhigh", "max"].map(
          (value) => [value, value],
        ),
      },
    );
    const external = el("div", "", "routing-external");
    const consent = field(
      external,
      "consent",
      "Send a short task description to the configured service",
      "checkbox",
      saved.send_task_description,
      "Only task text, up to 2048 bytes. Service charges may apply.",
    );
    const fallback = field(
      external,
      "fallback",
      "Use local rules if the service fails",
      "checkbox",
      saved.local_fallback,
    );
    const link = el("a", t("Configure Jev / Clef"));
    link.href = "#decision-assistance";
    external.append(link);
    if (!view.decisionServiceReady)
      external.append(
        el("p", t("Choose one service in Decision assistance first."), "muted"),
      );
    advanced.append(
      el("h3", t("Available models")),
      el(
        "small",
        t("Roles express your preference, not measured quota costs."),
        "muted",
      ),
    );
    const modelFields = [];
    for (const model of view.models || []) {
      const row = el("div", "", "routing-model");
      const included = field(
        row,
        `model-${modelFields.length}`,
        model.label || model.model,
        "checkbox",
        draft && draft.selectedModels !== null
          ? draft.selectedModels.includes(model.model)
          : !saved.allowed_models.length ||
              saved.allowed_models.includes(model.model),
      );
      const role = field(
        row,
        `role-${modelFields.length}`,
        model.model,
        "select",
        saved.model_roles[model.model] || "",
        t("Current role: {role}", {
          role: roleLabel(model.effectiveRole ?? model.role),
        }),
        {
          options: [
            ["", `${t("Use catalog role")} (${roleLabel(model.catalogRole)})`],
            ["economy", t("Economy")],
            ["balanced", t("Balanced")],
            ["capability", t("Capability")],
          ],
        },
      );
      modelFields.push({ model: model.model, included, role });
      advanced.append(row);
    }
    if (!modelFields.length)
      advanced.append(
        el(
          "p",
          t(
            "No verified models are available. Sign in and refresh the model catalog.",
          ),
          "muted",
        ),
      );
    advanced.append(
      api.button(t("Refresh model catalog"), async () => {
        if (api.busy || saving) return;
        try {
          const draft = collectDraft();
          const version = renderedVersion;
          await api.perform({ type: "routingRefreshModels" });
          restoredDraft = { ...draft, version };
          dirty = true;
          renderedKey = null;
          render(currentView, currentApi);
        } catch (error) {
          result.textContent = t(error.message);
          result.hidden = false;
        }
      }),
    );
    form.append(external, advanced);
    collectDraft = () => {
      const included = modelFields
        .filter((item) => item.included.checked)
        .map((item) => item.model);
      // Retain configured entries that the current identity cannot discover.
      const unseen = saved.allowed_models.filter(
        (name) => !modelFields.some((item) => item.model === name),
      );
      const roles = Object.assign(Object.create(null), saved.model_roles);
      for (const item of modelFields) {
        if (item.role.value) roles[item.model] = item.role.value;
        else delete roles[item.model];
      }
      // Null retains unrestricted selection; [] preserves an invalid empty draft.
      const selectedModels =
        !modelFields.length && draft
          ? draft.selectedModels
          : !saved.allowed_models.length &&
              included.length === modelFields.length &&
              !unseen.length
            ? null
            : [...included, ...unseen];
      return {
        selectedModels,
        config: {
          ...saved,
          mode: mode.value,
          source: source.value,
          main_tasks: main.checked,
          subagents: child.checked,
          preference: Number(bias.value),
          max_effort: effort.value,
          send_task_description: consent.checked,
          local_fallback: fallback.checked,
          allowed_models: selectedModels || [],
          model_roles: roles,
        },
      };
    };
    function collectPolicy() {
      const draft = collectDraft();
      if (draft.selectedModels?.length === 0)
        throw new Error(t("Select at least one model."));
      return draft.config;
    }
    const result = el("p", "", "message");
    result.hidden = true;
    result.setAttribute("role", "status");
    const actions = el("div", "", "actions");
    const save = api.button(t("Save selection policy"), () => {}, "primary");
    save.type = "submit";
    actions.append(save);
    form.append(actions, result);
    form.addEventListener("input", () => {
      dirty = true;
    });
    form.addEventListener("change", () => {
      dirty = true;
      updateSource();
    });
    function updateSource() {
      external.hidden = source.value !== "decision_service";
      consent.required =
        source.value === "decision_service" && mode.value !== "off";
    }
    updateSource();
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (api.busy || saving || !form.reportValidity()) return;
      saving = true;
      try {
        await api.perform({
          type: "routingSave",
          config: collectPolicy(),
          expectedVersion: version,
        });
        dirty = false;
        renderedKey = null;
      } catch (error) {
        result.textContent = t(error.message);
        result.className = "message error";
        result.hidden = false;
      } finally {
        saving = false;
        render(currentView, currentApi);
      }
    });
    root.append(form);
    const conflict = el("div", "", "routing-conflict message warning");
    conflict.hidden = view.userConfigVersion === version;
    conflict.append(
      el(
        "p",
        t("Settings changed elsewhere. Reload or save to check your version."),
      ),
      api.button(t("Reload policy"), () => {
        dirty = false;
        renderedKey = null;
        render(currentView, currentApi);
      }),
    );
    root.append(conflict);

    const simulation = el("details", "", "control-disclosure");
    simulation.append(el("summary", t("Local simulation")));
    const task = field(
      simulation,
      "task",
      "Describe a task",
      "text",
      taskDraft,
      "Simulation uses local rules and sends no external request.",
      { maxLength: 2048 },
    );
    task.addEventListener("input", () => {
      taskDraft = task.value;
    });
    const preview = el("p", "", "message");
    preview.setAttribute("role", "status");
    simulation.append(
      api.button(t("Simulate selection"), async () => {
        if (api.busy || saving || !task.value.trim()) return;
        try {
          const response = await api.operation({
            type: "routingPreview",
            task: task.value,
            config: collectPolicy(),
          });
          preview.textContent = response.data
            ? t("{model} · effort {effort}", response.data)
            : t("Keep current model; no compatible selection.");
        } catch (error) {
          preview.textContent = t(error.message);
        }
      }),
      preview,
    );
    root.append(simulation);
    const observation = view.lastDecision;
    const observationTime = Number.isFinite(observation?.decidedAt)
      ? new Date(observation.decidedAt * 1000).toLocaleString(
          window.AccountManagerMessages.locale(),
        )
      : "";
    root.append(
      el(
        "small",
        observation
          ? `${t("Last decision")}${observationTime ? ` (${observationTime})` : ""}: ${t(observation.applied ? "Applied" : "Suggested")} · ${t("{model} · effort {effort}", observation)}`
          : t("No decision recorded yet."),
        "muted",
      ),
    );
    for (const input of root.querySelectorAll("input, select, button"))
      input.disabled = api.busy;
  }
  window.AccountManagerRouting = Object.freeze({ render, configuration });
})();
