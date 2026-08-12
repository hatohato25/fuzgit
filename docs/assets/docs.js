/* ---------------------------------------------------------------------------
   fuzgit docs — language switch, collapsible table of contents, scroll spy.
   Plain ES5-compatible-ish DOM code. No dependencies, no network access.

   Language handling:
     - Both languages live in the same HTML file; blocks carry lang="en" /
       lang="ja" and are shown/hidden by CSS.
     - Without JavaScript the CSS default applies and the English text is
       readable, so this file only ever *upgrades* the page.
   --------------------------------------------------------------------------- */

(function () {
  "use strict";

  var STORAGE_KEY = "fuzgit-docs-lang";
  var SUPPORTED = ["en", "ja"];

  function isSupported(lang) {
    return SUPPORTED.indexOf(lang) !== -1;
  }

  function readStored() {
    try {
      return window.localStorage.getItem(STORAGE_KEY);
    } catch (error) {
      // Private mode / disabled storage: fall back to the in-page default.
      return null;
    }
  }

  function store(lang) {
    try {
      window.localStorage.setItem(STORAGE_KEY, lang);
    } catch (error) {
      // Persisting the choice is a convenience, not a requirement.
    }
  }

  function queryLang() {
    var match = /[?&]lang=([^&]+)/.exec(window.location.search);
    return match ? decodeURIComponent(match[1]) : null;
  }

  /** The language the page should start in: ?lang= wins over the stored one. */
  function initialLang() {
    var fromQuery = queryLang();
    if (isSupported(fromQuery)) {
      return fromQuery;
    }
    var stored = readStored();
    return isSupported(stored) ? stored : "en";
  }

  function apply(lang) {
    var root = document.documentElement;
    root.setAttribute("data-lang", lang);
    // Keep the document language honest for assistive technology.
    root.setAttribute("lang", lang);
  }

  // Applied as early as possible (this file is loaded with `defer`, and the
  // page also runs a tiny inline bootstrap in <head> to avoid a flash).
  var current = initialLang();
  apply(current);

  function setLang(lang, options) {
    if (!isSupported(lang)) {
      return;
    }
    current = lang;
    apply(lang);
    store(lang);
    syncButtons();
    if (options && options.updateUrl && window.history.replaceState) {
      var url = new URL(window.location.href);
      url.searchParams.set("lang", lang);
      window.history.replaceState(null, "", url.toString());
    }
  }

  var buttons = [];

  function syncButtons() {
    for (var i = 0; i < buttons.length; i += 1) {
      var button = buttons[i];
      var pressed = button.getAttribute("data-lang-value") === current;
      button.setAttribute("aria-pressed", pressed ? "true" : "false");
    }
  }

  function initLangSwitch() {
    buttons = Array.prototype.slice.call(
      document.querySelectorAll("[data-lang-value]")
    );
    for (var i = 0; i < buttons.length; i += 1) {
      buttons[i].addEventListener("click", function (event) {
        setLang(event.currentTarget.getAttribute("data-lang-value"), {
          updateUrl: true
        });
      });
    }
    syncButtons();
  }

  /* ------------------------------------------------------------------
     Table of contents: collapsible on narrow screens.
     ------------------------------------------------------------------ */

  function initToc() {
    var toggle = document.querySelector("[data-toc-toggle]");
    var toc = document.getElementById("toc");
    if (!toggle || !toc) {
      return;
    }

    var narrow = window.matchMedia("(max-width: 900px)");

    function collapseForViewport() {
      if (narrow.matches) {
        toc.hidden = true;
        toggle.setAttribute("aria-expanded", "false");
      } else {
        // On wide screens the sidebar is always visible.
        toc.hidden = false;
        toggle.setAttribute("aria-expanded", "true");
      }
    }

    collapseForViewport();

    if (narrow.addEventListener) {
      narrow.addEventListener("change", collapseForViewport);
    } else if (narrow.addListener) {
      narrow.addListener(collapseForViewport);
    }

    toggle.addEventListener("click", function () {
      var open = toc.hidden;
      toc.hidden = !open;
      toggle.setAttribute("aria-expanded", open ? "true" : "false");
    });

    toc.addEventListener("click", function (event) {
      var link = event.target.closest ? event.target.closest("a") : null;
      if (link && narrow.matches) {
        toc.hidden = true;
        toggle.setAttribute("aria-expanded", "false");
      }
    });
  }

  /* ------------------------------------------------------------------
     Scroll spy: highlight the section currently in view.
     ------------------------------------------------------------------ */

  function initScrollSpy() {
    var toc = document.getElementById("toc");
    if (!toc || !("IntersectionObserver" in window)) {
      return;
    }

    var links = {};
    var targets = [];
    var anchors = toc.querySelectorAll('a[href^="#"]');

    for (var i = 0; i < anchors.length; i += 1) {
      var id = anchors[i].getAttribute("href").slice(1);
      var section = document.getElementById(id);
      if (section) {
        links[id] = anchors[i];
        targets.push(section);
      }
    }

    var visible = {};
    var activeId = null;

    function refresh() {
      var next = null;
      for (var j = 0; j < targets.length; j += 1) {
        if (visible[targets[j].id]) {
          next = targets[j].id;
          break;
        }
      }
      if (!next || next === activeId) {
        return;
      }
      if (activeId && links[activeId]) {
        links[activeId].classList.remove("current");
        links[activeId].removeAttribute("aria-current");
      }
      activeId = next;
      links[activeId].classList.add("current");
      links[activeId].setAttribute("aria-current", "true");
    }

    var observer = new IntersectionObserver(
      function (entries) {
        for (var k = 0; k < entries.length; k += 1) {
          visible[entries[k].target.id] = entries[k].isIntersecting;
        }
        refresh();
      },
      { rootMargin: "-72px 0px -70% 0px", threshold: 0 }
    );

    for (var m = 0; m < targets.length; m += 1) {
      observer.observe(targets[m]);
    }
  }

  function init() {
    initLangSwitch();
    initToc();
    initScrollSpy();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
