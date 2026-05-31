(function () {
  "use strict";

  const sections = [
    { id: "introduction", title: "Introduction", href: "introduction.html", parts: ["Introduction", "Concepts"] },
    { id: "tutorials", title: "Tutorials", href: "getting-started.html", parts: ["Tutorials"] },
    { id: "guides", title: "Guides", href: "common-tasks.html", parts: ["How-to guides"] },
    { id: "reference", title: "Reference", href: "language-basics.html", parts: ["Reference"] },
    { id: "explanation", title: "Explanation", href: "host-boundary.html", parts: ["Explanation"] },
    { id: "operations", title: "Operations", href: "playground.html", parts: ["Operations"] },
  ];

  function normalizePath(pathname) {
    return pathname.replace(/^\/+/, "").replace(/\/index\.html$/, "/").replace(/^\.\//, "");
  }

  function relativeHref(target) {
    const depth = normalizePath(window.location.pathname)
      .split("/")
      .filter(Boolean)
      .slice(0, -1).length;
    return "../".repeat(depth) + target;
  }

  function activePart() {
    const active = document.querySelector(".chapter li.chapter-item a.active");
    if (!active) {
      return "Introduction";
    }

    let item = active.closest("li");
    while (item && item.previousElementSibling) {
      item = item.previousElementSibling;
      if (item.classList.contains("part-title")) {
        return item.textContent.trim();
      }
    }
    return "Introduction";
  }

  function installTopNav(currentSection) {
    const menu = document.querySelector("#menu-bar");
    if (!menu || document.querySelector(".harn-section-nav")) {
      return;
    }

    const nav = document.createElement("nav");
    nav.className = "harn-section-nav";
    nav.setAttribute("aria-label", "Documentation sections");

    for (const section of sections) {
      const link = document.createElement("a");
      link.href = relativeHref(section.href);
      link.textContent = section.title;
      if (section.id === currentSection.id) {
        link.className = "active";
        link.setAttribute("aria-current", "page");
      }
      nav.appendChild(link);
    }

    menu.appendChild(nav);
  }

  function scopeSidebar(currentSection) {
    const chapter = document.querySelector(".chapter");
    if (!chapter) {
      return;
    }

    const wanted = new Set(currentSection.parts);
    let currentPart = "Introduction";

    for (const item of chapter.children) {
      if (item.classList.contains("part-title")) {
        currentPart = item.textContent.trim();
      }
      item.classList.toggle("harn-sidebar-hidden", !wanted.has(currentPart));
    }
  }

  document.addEventListener("DOMContentLoaded", function () {
    const part = activePart();
    const currentSection =
      sections.find((section) => section.parts.includes(part)) || sections[0];
    document.body.dataset.harnDocsSection = currentSection.id;
    installTopNav(currentSection);
    scopeSidebar(currentSection);
  });
})();
