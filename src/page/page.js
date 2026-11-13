// The installer's page: asks the program for its state about every second,
// shows it, and sends back what's typed. The key after the address's # goes
// with every question, and nowhere else (src/page.rs).
"use strict";

(() => {
  const key = decodeURIComponent(location.hash.slice(1));
  const card = document.getElementById("card");
  const stepper = document.getElementById("stepper").children;
  const lost = document.getElementById("lost");

  /** How often to ask, and how long without an answer before saying so. */
  const EVERY_MS = 800;
  const LOST_MS = 15000;

  let shown = null;
  let sending = false;
  let failingSince = 0;
  let over = false;

  if (!/^[A-Za-z0-9_-]{16,128}$/.test(key)) {
    card.replaceChildren(document.getElementById("no-key").content.cloneNode(true));
    return;
  }

  function poll() {
    fetch("/state", { headers: { "X-Atelier-Key": key }, cache: "no-store" })
      .then((response) => {
        if (!response.ok) throw new Error(`status ${response.status}`);
        return response.json();
      })
      .then((state) => {
        failingSince = 0;
        lost.hidden = true;
        if (!shown || state.seq !== shown.seq) show(state);
      })
      .catch(() => {
        if (!failingSince) failingSince = Date.now();
        else if (!over && Date.now() - failingSince > LOST_MS) lost.hidden = false;
      })
      .finally(() => {
        if (!over) setTimeout(poll, EVERY_MS);
      });
  }

  // Time in Settings isn't time the program didn't answer.
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) failingSince = 0;
  });

  function show(state) {
    const view = state.view;
    for (let index = 0; index < stepper.length; index++) {
      stepper[index].className = index < state.stage ? "done" : index === state.stage ? "now" : "";
    }
    const moving = shown && shown.view.screen === view.screen && !state.asking && !shown.asking;
    if (moving && (view.screen === "installing" || view.screen === "busy")) {
      fill(card, view);
    } else {
      build(view, state.asking, shown && shown.view.screen === view.screen);
    }
    shown = state;
    sending = false;
    if (view.screen === "done" || view.screen === "stopped") over = true;
  }

  function build(view, asking, again) {
    // What was typed, kept when the same screen comes back, with a problem.
    const kept = {};
    if (again) {
      for (const input of card.querySelectorAll("input[data-keep]")) kept[input.name] = input.value;
    }
    const screen = document.getElementById(view.screen);
    if (!screen) return;
    const content = screen.content.cloneNode(true);
    fill(content, view);
    card.replaceChildren(content);
    for (const [name, value] of Object.entries(kept)) {
      const input = card.querySelector(`input[name="${name}"]`);
      if (input) input.value = value;
    }
    wire(view);
    busy(!asking);
  }

  function field(root, name) {
    return root.querySelector(`[data-field="${name}"]`);
  }

  function part(root, name) {
    return root.querySelector(`[data-part="${name}"]`);
  }

  function say(root, name, text) {
    const element = field(root, name);
    if (!element) return;
    element.textContent = text || "";
    element.hidden = !text;
  }

  function fill(root, view) {
    say(root, "problem", view.problem);
    switch (view.screen) {
      case "busy":
        say(root, "title", view.title);
        part(root, "bar").hidden = !view.bar;
        part(root, "wait").hidden = !!view.bar;
        if (view.bar) bar(root, view.bar);
        break;
      case "pair":
        say(root, "code", view.code.replace(/^(\d{3})(\d{3})$/, "$1 $2"));
        break;
      case "sign-in": {
        const password = part(root, "password");
        password.hidden = !view.password;
        password.querySelector("input").disabled = !view.password;
        break;
      }
      case "code":
        // Not drawn yet: a code by text message.
        if (view.texted !== null) {
          say(root, "sent", view.texted
            ? `Apple texted a six-digit code to ${view.texted}. When it comes, note it, then come back here.`
            : "Apple texted a six-digit code. When it comes, note it, then come back here.");
        }
        root.querySelector('[data-action="text"]').hidden = !view.by_text;
        root.querySelector('[data-action="devices"]').hidden = !view.to_devices;
        break;
      case "question": {
        say(root, "title", view.title);
        const text = field(root, "text");
        text.replaceChildren(...view.text.map((line) => {
          const paragraph = document.createElement("p");
          paragraph.textContent = line;
          return paragraph;
        }));
        field(root, "options").replaceChildren(...view.options.map((option, index) => {
          const label = document.createElement("label");
          const radio = document.createElement("input");
          radio.type = "radio";
          radio.name = "pick";
          radio.value = String(index);
          label.append(radio, document.createTextNode(option));
          return label;
        }));
        say(root, "yes", view.yes);
        say(root, "no", view.no);
        break;
      }
      case "review":
        say(root, "release", view.release);
        say(root, "size", view.size);
        say(root, "app-id", view.app_id);
        break;
      case "installing":
        bar(root, view.bar);
        root.querySelector('[data-tick="signing"]').hidden = view.signed;
        root.querySelector('[data-tick="signed"]').hidden = !view.signed;
        root.querySelector('[data-tick="installing"]').hidden = !view.signed;
        break;
      case "done":
        part(root, "handed").hidden = !view.handed;
        break;
      case "stopped":
        say(root, "what", view.what);
        field(root, "next").replaceChildren(...view.next.map((line) => {
          const item = document.createElement("li");
          item.textContent = line;
          return item;
        }));
        break;
    }
  }

  function bar(root, progress) {
    field(root, "bar").style.width = `${progress.percent}%`;
    say(root, "bar-text", progress.text);
    say(root, "percent", `${progress.percent}%`);
  }

  function wire(view) {
    const form = card.querySelector("form[data-form]");
    if (form) {
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        const typed = {};
        for (const input of form.querySelectorAll("input[name]")) {
          if (!input.disabled) typed[input.name] = input.value;
        }
        if (view.screen === "code") typed.code = typed.code.replace(/\D/g, "");
        answer(typed);
      });
    }
    const digits = card.querySelector(".digits-box input");
    if (digits) wireDigits(digits);
    for (const button of card.querySelectorAll("[data-action]")) {
      button.addEventListener("click", () => answer({ action: button.dataset.action }));
    }
    for (const button of card.querySelectorAll("[data-answer]")) {
      button.addEventListener("click", () => {
        if (button.dataset.answer === "no") return answer({ yes: false });
        const picked = card.querySelector('input[name="pick"]:checked');
        if (view.options && view.options.length && !picked) return;
        answer(picked ? { yes: true, pick: Number(picked.value) } : { yes: true });
      });
    }
  }

  /** Apple's code: one field, shown as six boxes. */
  function wireDigits(input) {
    const boxes = card.querySelectorAll(".digits i");
    const draw = () => {
      input.value = input.value.replace(/\D/g, "").slice(0, 6);
      boxes.forEach((box, index) => {
        box.textContent = input.value[index] || "";
        box.classList.toggle("focus",
          document.activeElement === input && index === Math.min(input.value.length, 5));
      });
    };
    for (const event of ["input", "focus", "blur"]) input.addEventListener(event, draw);
    draw();
  }

  function busy(on) {
    for (const control of card.querySelectorAll("button, input")) {
      if (on) {
        control.dataset.wasDisabled = control.disabled ? "1" : "";
        control.disabled = true;
      } else if (control.dataset.wasDisabled !== undefined) {
        control.disabled = control.dataset.wasDisabled === "1";
        delete control.dataset.wasDisabled;
      }
    }
  }

  function answer(fields) {
    if (sending || !shown || !shown.asking) return;
    sending = true;
    busy(true);
    const seq = shown.seq;
    fetch("/answer", {
      method: "POST",
      headers: { "X-Atelier-Key": key, "Content-Type": "application/json" },
      body: JSON.stringify({ seq, answer: fields }),
      cache: "no-store",
    })
      .then((response) => response.json())
      .then((reply) => {
        if (!reply.taken && shown && shown.seq === seq) {
          sending = false;
          busy(false);
        }
      })
      .catch(() => {
        if (shown && shown.seq === seq) {
          sending = false;
          busy(false);
        }
      });
  }

  poll();
})();
