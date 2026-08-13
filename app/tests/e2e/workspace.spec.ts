import { expect, test } from "@playwright/test";

const routes = ["Generate", "Projects", "Transcribe", "Voices", "Models", "Live", "Compare", "Benchmarks", "History", "Settings", "About"];
const mobileDirectRoutes = new Set(["Generate", "Projects", "Transcribe", "Voices"]);

async function openRoute(page: import("@playwright/test").Page, route: string) {
  if (page.viewportSize()!.width <= 820) {
    const dock = page.locator(".mobile-nav");
    if (mobileDirectRoutes.has(route)) {
      await dock.getByRole("button", { name: route, exact: true }).click();
    } else {
      await dock.getByRole("button", { name: "More navigation" }).click();
      await page.locator(".mobile-more-menu").getByRole("button", { name: route, exact: true }).click();
    }
  } else {
    await page.locator(".sidebar").getByRole("button", { name: route, exact: true }).click();
  }
  await expect(page.getByRole("heading", { name: route })).toBeVisible();
}

test("workspace is explicit about preview and native capability boundaries", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("Browser preview")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Generate" })).toBeVisible();
  await openRoute(page, "Live");
  await expect(page.getByRole("heading", { name: "Live" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Record" })).toBeDisabled();
  await expect(page.getByText("No input")).toBeVisible();
  await expect(page.getByText("No output")).toBeVisible();
  await expect(page.locator("body")).not.toHaveCSS("overflow-x", "scroll");
});

test("implemented routes remain usable at the target viewport", async ({ page }) => {
  await page.goto("/");
  for (const route of ["Projects", "Transcribe", "Voices", "Models", "Live", "Compare", "Benchmarks", "History"]) {
    await openRoute(page, route);
  }
});

test("workspace controls do not collide at the target viewport", async ({ page }) => {
  await page.goto("/");
  for (const route of routes) {
    await openRoute(page, route);
    const collisions = await page.locator("main").evaluate((main) => {
      const candidates = [...main.querySelectorAll<HTMLElement>(
        "button, input, textarea, [role=combobox], .panel, .page-header, .status-text",
      )].filter((element) => {
        const style = getComputedStyle(element);
        const box = element.getBoundingClientRect();
        return style.display !== "none" && style.visibility !== "hidden" && box.width > 0 && box.height > 0;
      });
      const overlaps: string[] = [];
      for (let leftIndex = 0; leftIndex < candidates.length; leftIndex += 1) {
        for (let rightIndex = leftIndex + 1; rightIndex < candidates.length; rightIndex += 1) {
          const left = candidates[leftIndex];
          const right = candidates[rightIndex];
          if (left.contains(right) || right.contains(left)) continue;
          const a = left.getBoundingClientRect();
          const b = right.getBoundingClientRect();
          const overlapX = Math.min(a.right, b.right) - Math.max(a.left, b.left);
          const overlapY = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
          if (overlapX > 2 && overlapY > 2) {
            overlaps.push(`${left.getAttribute("aria-label") || left.className} overlaps ${right.getAttribute("aria-label") || right.className}`);
          }
        }
      }
      return overlaps;
    });
    expect(collisions, `${route} has overlapping controls`).toEqual([]);
  }
});

test("timed transcript corrections remain compact and saveable", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Transcribe");

  await expect(page.getByText("EN / 99.6%")).toBeVisible();
  await expect(page.getByText("6 aligned")).toBeVisible();
  const segment = page.getByRole("textbox", { name: "Transcript segment 1" });
  await segment.fill("Corrected preview transcript.");
  await page.getByRole("button", { name: "Save correction" }).click();
  await expect(page.getByText("Revision 1 saved")).toBeVisible();
  await expect(page.getByText("1 correction revision")).toBeVisible();

  const bounds = await page.locator(".transcript-editor").evaluate((panel) => ({
    clientWidth: panel.clientWidth,
    scrollWidth: panel.scrollWidth,
    right: panel.getBoundingClientRect().right,
    viewport: window.innerWidth,
  }));
  expect(bounds.scrollWidth).toBeLessThanOrEqual(bounds.clientWidth + 1);
  expect(bounds.right).toBeLessThanOrEqual(bounds.viewport + 1);
});

test("speaker separation evidence, labels, and turn playback stay compact", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Transcribe");

  await expect(page.getByText("Provisional clustering")).toBeVisible();
  await expect(page.getByText("Overlap not detected")).toBeVisible();
  await expect(page.getByText("No turn confidence")).toBeVisible();
  await page.getByRole("textbox", { name: "Name Speaker 1" }).fill("Host");
  await page.getByRole("button", { name: "Save speaker labels" }).click();
  await expect(page.getByText("Speaker labels revision 1 saved")).toBeVisible();
  await expect(page.getByRole("button", { name: "Play speaker turn 1" })).toBeVisible();

  const overflow = await page.getByRole("region", { name: "Speaker separation", exact: true }).evaluate((section) => ({
    clientWidth: section.clientWidth,
    scrollWidth: section.scrollWidth,
    left: section.getBoundingClientRect().left,
    right: section.getBoundingClientRect().right,
    viewportWidth: window.innerWidth,
  }));
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth + 1);
  expect(overflow.left).toBeGreaterThanOrEqual(-1);
  expect(overflow.right).toBeLessThanOrEqual(overflow.viewportWidth + 1);
});

test("forced alignment stays compact and is invalidated by a correction", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Transcribe");
  const alignment = page.getByRole("region", { name: "Forced word alignment" });
  await expect(alignment.getByText("Scores uncalibrated")).toBeVisible();
  await expect(alignment.getByText("Revision 0")).toBeVisible();
  await expect(alignment.locator(".alignment-word-rail button")).toHaveCount(6);

  await page.getByRole("textbox", { name: "Transcript segment 1" }).fill("Corrected preview transcript.");
  await page.getByRole("button", { name: "Save correction" }).click();
  await expect(alignment.getByText("Stale after correction")).toBeVisible();
  await expect(alignment.getByRole("button", { name: "Align correction" })).toBeEnabled();
  const bounds = await alignment.evaluate((element) => ({ client: element.clientWidth, scroll: element.scrollWidth }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.client + 1);
});

test("collapsed navigation keeps unambiguous route names", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await page.goto("/");

  for (const route of routes) {
    const button = page.locator(".sidebar").getByRole("button", { name: route, exact: true });
    await expect(button).toHaveCount(1);
    await expect(button).toHaveAccessibleName(route);
  }
});

test("phone workspace never expands beyond the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");

  for (const route of routes) {
    await openRoute(page, route);
    const dimensions = await page.evaluate(() => ({
      documentWidth: document.documentElement.scrollWidth,
      viewportWidth: document.documentElement.clientWidth,
    }));
    expect(dimensions.documentWidth, `${route} widened the phone workspace`).toBeLessThanOrEqual(dimensions.viewportWidth + 1);
    if (route === "Voices") {
      const table = page.locator(".voice-table");
      expect(await table.evaluate((element) => element.scrollWidth <= element.clientWidth + 1), "Voices table requires horizontal scrolling").toBe(true);
    }
    if (route === "Models") {
      const inspector = page.locator(".model-inspector");
      expect(await inspector.evaluate((element) => element.scrollWidth <= element.clientWidth + 1), "Model inspector overflows its panel").toBe(true);
    }
  }
});

test("narrow phone keeps primary selectors and inspector text readable", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 720 });
  await page.goto("/");

  const route = page.getByRole("group", { name: "Model route" });
  const routeBounds = await route.evaluate((element) => ({
    left: element.getBoundingClientRect().left,
    right: element.getBoundingClientRect().right,
    viewport: window.innerWidth,
    clipped: [...element.querySelectorAll("button")].filter((button) => button.scrollWidth > button.clientWidth + 1).map((button) => button.textContent),
  }));
  expect(routeBounds.left).toBeGreaterThanOrEqual(0);
  expect(routeBounds.right).toBeLessThanOrEqual(routeBounds.viewport + 1);
  expect(routeBounds.clipped).toEqual([]);

  await openRoute(page, "Models");
  const summary = page.locator(".model-inspector-main p");
  expect(await summary.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);

  await openRoute(page, "Transcribe");
  const alignment = page.locator(".alignment-word-rail");
  const alignmentBounds = await alignment.evaluate((element) => ({ client: element.clientWidth, scroll: element.scrollWidth }));
  expect(alignmentBounds.scroll).toBeLessThanOrEqual(alignmentBounds.client + 1);
});

test("tablet model registry fits without horizontal scrolling", async ({ page }) => {
  await page.setViewportSize({ width: 768, height: 900 });
  await page.goto("/");
  await openRoute(page, "Models");

  const bounds = await page.locator(".model-table-panel").evaluate((panel) => {
    const scroller = panel.querySelector<HTMLElement>(".table-scroll")!;
    const table = panel.querySelector<HTMLElement>(".model-table")!;
    return {
      panelRight: panel.getBoundingClientRect().right,
      tableRight: table.getBoundingClientRect().right,
      clientWidth: scroller.clientWidth,
      scrollWidth: scroller.scrollWidth,
      viewportWidth: window.innerWidth,
    };
  });
  expect(bounds.scrollWidth).toBeLessThanOrEqual(bounds.clientWidth + 1);
  expect(bounds.tableRight).toBeLessThanOrEqual(bounds.panelRight + 1);
  expect(bounds.panelRight).toBeLessThanOrEqual(bounds.viewportWidth + 1);
});

test("phone navigation never covers workspace controls", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");

  for (const route of routes) {
    await openRoute(page, route);
    const initialClearance = await page.evaluate(() => {
      const content = document.querySelector<HTMLElement>(".app-content")!.getBoundingClientRect();
      const navTop = document.querySelector<HTMLElement>(".mobile-nav")!.getBoundingClientRect().top;
      return { contentBottom: content.bottom, navTop };
    });
    expect(initialClearance.contentBottom, `${route} workspace extends behind mobile navigation`).toBeLessThanOrEqual(initialClearance.navTop + 1);

    await page.locator(".app-content").evaluate((content) => { content.scrollTop = content.scrollHeight; });
    const clearance = await page.evaluate(() => {
      const content = document.querySelector<HTMLElement>(".app-content")!;
      const contentBox = content.getBoundingClientRect();
      const navTop = document.querySelector<HTMLElement>(".mobile-nav")!.getBoundingClientRect().top;
      const visibleControls = [...document.querySelectorAll<HTMLElement>("main button:not([disabled]), main input:not([disabled]), main textarea:not([disabled]), main [role=combobox]")]
        .filter((element) => {
          const style = getComputedStyle(element);
          const box = element.getBoundingClientRect();
          return style.display !== "none" && style.visibility !== "hidden" && box.bottom > contentBox.top && box.top < contentBox.bottom;
        });
      const obscured = visibleControls.filter((element) => element.getBoundingClientRect().bottom > navTop + 1)
        .map((element) => (element.innerText || element.getAttribute("aria-label") || element.tagName).trim().slice(0, 60));
      return { obscured, contentBottom: contentBox.bottom, navTop };
    });
    expect(clearance.obscured, `${route} actions overlap mobile navigation`).toEqual([]);
    expect(clearance.contentBottom, `${route} workspace moved behind mobile navigation`).toBeLessThanOrEqual(clearance.navTop + 1);
  }
});

test("compact model labels remain readable in the registry", async ({ page }) => {
  await page.setViewportSize({ width: 768, height: 900 });
  await page.goto("/");
  await openRoute(page, "Models");
  const wavlmRow = page.locator(".model-table tbody tr").filter({ hasText: "wavlm-base-plus-sv" });
  await wavlmRow.click();
  await expect(wavlmRow.locator(".engine-cell")).toHaveText("WavLM");
  await expect(page.locator(".model-inspector-facts")).toContainText("Speaker similarity");
});

test("phone voice consent dialog fits above the navigation", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await openRoute(page, "Voices");
  await page.getByRole("button", { name: "Add voice profile", exact: true }).click();

  const bounds = await page.getByRole("dialog", { name: "Add voice profile" }).evaluate((dialog) => {
    const box = dialog.getBoundingClientRect();
    const navTop = document.querySelector<HTMLElement>(".mobile-nav")!.getBoundingClientRect().top;
    return { left: box.left, right: box.right, bottom: box.bottom, navTop, scrollWidth: dialog.scrollWidth, clientWidth: dialog.clientWidth };
  });
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(390);
  expect(bounds.bottom).toBeLessThanOrEqual(bounds.navTop);
  expect(bounds.scrollWidth).toBeLessThanOrEqual(bounds.clientWidth + 1);
  await expect(page.getByRole("button", { name: "Create profile" })).toBeDisabled();
});

test("projects support create, edit, undo, redo, and local save", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Projects");
  await page.getByRole("button", { name: "New project" }).click();
  await page.getByLabel("Project name").fill("Responsive release narration");
  await page.getByLabel("Script").fill("A durable chapter edited through the real workspace.");
  await page.getByRole("button", { name: "Undo" }).click();
  await expect(page.getByLabel("Script")).toHaveValue("");
  await page.getByRole("button", { name: "Redo" }).click();
  await expect(page.getByLabel("Script")).toHaveValue("A durable chapter edited through the real workspace.");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved locally", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Render stale (1)" }).click();
  await expect(page.getByText(/0\/1 complete.*queued/i)).toBeVisible();
  await page.getByRole("button", { name: "Cancel project rendering" }).click();
  await expect(page.getByText("Project rendering cancelled", { exact: true })).toBeVisible();
});

test("browser preview reports model installation boundary without a stranded dialog", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Models");
  await page.getByRole("button", { name: /More actions for chatterbox-turbo/i }).click();
  await page.getByRole("menuitem", { name: "Install model" }).click();
  await expect(page.getByRole("status")).toContainText("Model installation is available in the soundAr desktop app");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("every route keeps controls inside its layout in both themes", async ({ page }) => {
  await page.goto("/");

  for (const theme of ["dark", "light"] as const) {
    if (theme === "light") await page.getByRole("button", { name: "Cream light" }).filter({ visible: true }).click();

    for (const route of routes) {
      await openRoute(page, route);

      const overflow = await page.locator("main").evaluate((main, currentRoute) => {
        const selectors = [
          ".panel", ".field", ".compact-field", ".page-header", ".data-toolbar",
          ".segmented", ".metric-strip", ".output-block", "button", "textarea",
          "input:not([type=range])", "[role=combobox]", ".compact-definition-list > div",
          ".compact-definition-list dd", ".about-identity", ".about-version",
        ].join(",");

        return [...main.querySelectorAll<HTMLElement>(selectors)].flatMap((element) => {
          const box = element.getBoundingClientRect();
          const clipped = element.scrollWidth > element.clientWidth + 2 || element.scrollHeight > element.clientHeight + 2;
          const outside = box.left < -1 || box.right > window.innerWidth + 1;
          if (!clipped && !outside) return [];
          return [{
            route: currentRoute,
            element: element.className || element.tagName,
            text: (element.innerText || element.getAttribute("aria-label") || "").trim().replace(/\s+/g, " ").slice(0, 80),
            client: [element.clientWidth, element.clientHeight],
            scroll: [element.scrollWidth, element.scrollHeight],
          }];
        });
      }, route);

      expect(overflow, `${theme} ${route} overflow`).toEqual([]);

      if (route === "About") {
        await expect(page.getByText("Version 0.3.0", { exact: true })).toBeVisible();
        const runtimeDetails = page.getByLabel("Runtime details");
        await expect(runtimeDetails.getByText(/NVIDIA GeForce|No compatible GPU/)).toBeVisible();
      }
    }
  }
});

test("hostile runtime text wraps without covering actions", async ({ page }) => {
  await page.goto("/");
  const hostile = "Could-not-start-the-Python-runtime-at-/home/a-user-with-an-extremely-long-name/.soundAr/runtimes/speaker-verification/bin/python-because-the-local-runtime-package-is-incomplete";

  for (const viewport of [{ width: 1220, height: 720 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(viewport);
    await openRoute(page, "Generate");
    await page.locator(".composer-state").evaluate((element, message) => {
      element.textContent = message;
      element.className = "status-text status-danger composer-state";
    }, hostile);

    const result = await page.locator(".composer-footer").evaluate((footer) => {
      const footerBox = footer.getBoundingClientRect();
      const status = footer.querySelector<HTMLElement>(".status-text")!;
      const statusBox = status.getBoundingClientRect();
      const buttons = [...footer.querySelectorAll<HTMLElement>("button")];
      const overlaps = buttons.filter((button) => {
        const box = button.getBoundingClientRect();
        return Math.min(statusBox.right, box.right) - Math.max(statusBox.left, box.left) > 1
          && Math.min(statusBox.bottom, box.bottom) - Math.max(statusBox.top, box.top) > 1;
      });
      return {
        footerRight: footerBox.right,
        statusRight: statusBox.right,
        viewportRight: window.innerWidth,
        scrollWidth: status.scrollWidth,
        clientWidth: status.clientWidth,
        overlaps: overlaps.map((button) => button.innerText),
      };
    });

    expect(result.statusRight).toBeLessThanOrEqual(result.footerRight + 1);
    expect(result.footerRight).toBeLessThanOrEqual(result.viewportRight + 1);
    expect(result.scrollWidth).toBeLessThanOrEqual(result.clientWidth + 2);
    expect(result.overlaps).toEqual([]);
  }
});

test("custom dropdowns stay compact and selectable", async ({ page }) => {
  await page.goto("/");
  const model = page.getByRole("combobox", { name: "Model" });
  await model.click();
  const option = page.getByRole("option").first();
  await expect(option).toBeVisible();
  await expect(option).toHaveCSS("font-size", "11px");
  await option.click();
  await expect(model).toHaveCSS("font-size", "11px");

  await page.getByRole("button", { name: "Batch" }).click();
  await expect(page.getByLabel("Parallel jobs")).toBeVisible();
  await expect(page.getByRole("button", { name: "Start batch" })).toBeVisible();
});

test("mobile routes reset scroll and overlays stay above navigation", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");

  const content = page.locator(".app-content");
  await content.evaluate((element) => { element.scrollTop = element.scrollHeight; });
  await openRoute(page, "Projects");
  expect(await content.evaluate((element) => element.scrollTop)).toBe(0);

  await openRoute(page, "Generate");
  const output = page.getByRole("combobox", { name: "Output format" });
  await output.evaluate((element) => element.scrollIntoView({ block: "end" }));
  await output.click();
  const dropdownBounds = await page.locator(".dropdown-menu").evaluate((menu) => {
    const box = menu.getBoundingClientRect();
    const workspace = document.querySelector<HTMLElement>(".app-content")!.getBoundingClientRect();
    return { top: box.top, bottom: box.bottom, workspaceTop: workspace.top, workspaceBottom: workspace.bottom };
  });
  expect(dropdownBounds.top).toBeGreaterThanOrEqual(dropdownBounds.workspaceTop - 1);
  expect(dropdownBounds.bottom).toBeLessThanOrEqual(dropdownBounds.workspaceBottom + 1);

  await page.keyboard.press("Escape");
  await page.getByLabel("Script").fill("Mobile history action proof.");
  await page.getByRole("button", { name: "Queue audio" }).click();
  await openRoute(page, "History");
  await page.getByRole("button", { name: /More actions for/i }).first().click();
  const menuBounds = await page.locator(".row-action-popover").evaluate((menu) => {
    const box = menu.getBoundingClientRect();
    const workspace = document.querySelector<HTMLElement>(".app-content")!.getBoundingClientRect();
    return { top: box.top, bottom: box.bottom, workspaceTop: workspace.top, workspaceBottom: workspace.bottom };
  });
  expect(menuBounds.top).toBeGreaterThanOrEqual(menuBounds.workspaceTop - 1);
  expect(menuBounds.bottom).toBeLessThanOrEqual(menuBounds.workspaceBottom + 1);
});

test("generate does not expose inert export controls", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Open output" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Export audio" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Change" })).toHaveCount(0);
  await openRoute(page, "Settings");
  await expect(page.getByRole("button", { name: "Choose directory" })).toHaveCount(0);
});

test("preview batch reaches a coherent completed state", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Batch" }).click();
  await page.getByLabel("Script").fill("First preview row.\nSecond preview row.");
  await page.getByRole("button", { name: "Start batch" }).click();
  await expect(page.getByText("2/2 complete")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByRole("button", { name: "Cancel batch" })).toHaveCount(0);
  await page.getByRole("button", { name: "Show batch rows" }).click();
  const rows = page.getByLabel(/Batch .* rows/).locator(".batch-item-row");
  await expect(rows).toHaveCount(2);
  await expect(rows.filter({ hasText: "completed" })).toHaveCount(2);
});

test("queue priority is compact and remains visible on a batch", async ({ page }) => {
  await page.goto("/");
  const priority = page.getByRole("combobox", { name: "Queue priority" });
  await expect(priority).toHaveCSS("font-size", "11px");
  await priority.click();
  await page.getByRole("option", { name: "Urgent" }).click();
  await page.getByRole("button", { name: "Batch" }).click();
  await page.getByLabel("Script").fill("Priority preview row.");
  await page.getByRole("button", { name: "Start batch" }).click();
  await expect(page.locator(".batch-queue-entry").first()).toContainText("urgent");
});

test("Voice Lab remains compact with the reference editor expanded", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Voices");
  await page.getByRole("button", { name: "Edit" }).click();
  await expect(page.getByLabel("Reference waveform")).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply as new revision" })).toBeDisabled();

  const overflow = await page.locator(".voice-inspector").evaluate((inspector) => {
    return [...inspector.querySelectorAll<HTMLElement>("button, textarea, input, [role=combobox], .reference-waveform")]
      .filter((element) => element.scrollWidth > element.clientWidth + 2)
      .map((element) => ({ element: element.className || element.tagName, text: element.innerText || element.getAttribute("aria-label") }));
  });
  expect(overflow).toEqual([]);
});

test("voice table keeps preview primary and secondary actions in a compact menu", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Voices");

  const row = page.locator(".voice-table tbody tr").filter({ hasText: "Mara" }).first();
  await expect(row.getByRole("button", { name: "Play Mara" })).toBeVisible();
  await expect(row.getByRole("button", { name: "More actions for Mara" })).toBeVisible();
  await expect(row.locator("button")).toHaveCount(2);

  await row.getByRole("button", { name: "More actions for Mara" }).click();
  const menu = page.getByRole("menu", { name: "More actions for Mara" });
  await expect(menu.getByRole("menuitem", { name: "View details" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Use voice" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Delete profile" })).toBeVisible();

  const bounds = await menu.evaluate((element) => {
    const box = element.getBoundingClientRect();
    return { left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: window.innerWidth, height: window.innerHeight };
  });
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(bounds.width);
  expect(bounds.top).toBeGreaterThanOrEqual(0);
  expect(bounds.bottom).toBeLessThanOrEqual(bounds.height);
});

test("model health reports truthful worker lifecycle state without overflow", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Models");
  await page.getByRole("button", { name: "Health" }).click();
  await expect(page.getByText(/files ready.*preview on browser.*0 warm.*0 starts.*no failures/i)).toBeVisible();
  const status = page.locator(".model-inspector > .status-text");
  expect(await status.evaluate((element) => element.scrollWidth <= element.clientWidth + 2)).toBe(true);
});

test("benchmark evidence is explicit and cannot be fabricated in browser preview", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Benchmarks");
  await expect(page.getByText("Mean WER", { exact: true })).toBeVisible();
  await expect(page.getByText("Mean CER", { exact: true })).toBeVisible();
  const run = page.getByRole("button", { name: "Run measured suite" });
  await expect(run).toBeDisabled();
  await expect(run).toHaveAttribute("title", "Measured suites require the desktop runtime");
  await expect(page.getByText(/native runtime verifies cold\/warm state.*exact generated artifact/i)).toBeVisible();
});

test("blind comparison matrix renders, reviews, reveals, and promotes without overflow", async ({ page }) => {
  await page.goto("/");
  await openRoute(page, "Compare");
  await page.getByRole("button", { name: "4 takes" }).click();
  await expect(page.getByRole("combobox", { name: "Take D" })).toBeVisible();
  await page.getByRole("button", { name: "Render takes" }).click();

  const takes = page.getByLabel("Comparison takes").locator(".compare-side");
  await expect(takes).toHaveCount(4);
  await expect(takes.filter({ hasText: "Identity hidden" })).toHaveCount(4);
  await expect(page.getByRole("button", { name: "Reveal identities" })).toBeVisible();

  await takes.first().getByTitle("5 stars").click();
  await page.getByRole("button", { name: "Tie", exact: true }).click();
  await expect(page.getByText("Marked as a tie")).toBeVisible();
  await takes.first().getByRole("button", { name: "Winner" }).click();
  await expect(page.getByRole("button", { name: "Tie", exact: true })).toHaveAttribute("aria-pressed", "false");
  await takes.first().getByRole("button", { name: "Promote" }).click();
  await expect(page.getByText("Take A promoted to History")).toBeVisible();
  await page.getByRole("button", { name: "Reveal identities" }).click();
  await expect(takes.filter({ hasText: "Identity hidden" })).toHaveCount(0);

  const overflow = await page.getByLabel("Comparison takes").evaluate((matrix) => [...matrix.querySelectorAll<HTMLElement>(".compare-side, button, textarea, [role=combobox]")].filter((element) => element.scrollWidth > element.clientWidth + 2 || element.getBoundingClientRect().right > window.innerWidth + 1).map((element) => ({ element: element.className, text: element.innerText.slice(0, 60) })));
  expect(overflow).toEqual([]);
});

test("History filters and compact artifact actions remain usable", async ({ page }) => {
  await page.goto("/");
  await page.getByLabel("Script").fill("History workbench browser proof.");
  await page.getByRole("button", { name: "Queue audio" }).click();
  await expect(page.getByText("History workbench browser proof", { exact: false }).first()).toBeVisible({ timeout: 10_000 });
  await openRoute(page, "History");

  await expect(page.getByRole("combobox", { name: "Model filter" })).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Voice filter" })).toBeVisible();
  await page.getByRole("combobox", { name: "Artifact filter" }).click();
  await page.getByRole("option", { name: "Unavailable" }).click();
  await expect(page.getByText("History workbench browser proof", { exact: false }).first()).toBeVisible();

  await page.getByRole("button", { name: /More actions for History workbench browser proof/i }).click();
  await page.getByRole("menuitem", { name: "Add favorite" }).click();
  await page.getByRole("button", { name: "Favorites", exact: true }).click();
  await page.getByRole("button", { name: /More actions for History workbench browser proof/i }).click();
  await expect(page.getByRole("menuitem", { name: "Duplicate artifact" })).toBeDisabled();
  await expect(page.getByRole("menuitem", { name: "Export copy" })).toBeDisabled();

  const bounds = await page.locator(".history-toolbar, .row-action-popover").evaluateAll((elements) => elements.map((element) => {
    const box = element.getBoundingClientRect();
    return { left: box.left, right: box.right, width: window.innerWidth, scrollWidth: element.scrollWidth, clientWidth: element.clientWidth };
  }));
  expect(bounds.every((box) => box.left >= -1 && box.right <= box.width + 1 && box.scrollWidth <= box.clientWidth + 2)).toBe(true);
});

test("phone History keeps artifact actions visible without horizontal scrolling", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.getByLabel("Script").fill("Phone history controls stay visible.");
  await page.getByRole("button", { name: "Queue audio" }).click();
  await openRoute(page, "History");

  const table = page.locator(".history-table");
  expect(await table.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  const actions = page.locator(".history-actions").first();
  const bounds = await actions.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const workspace = document.querySelector<HTMLElement>(".app-content")!.getBoundingClientRect();
    return { left: box.left, right: box.right, workspaceLeft: workspace.left, workspaceRight: workspace.right };
  });
  expect(bounds.left).toBeGreaterThanOrEqual(bounds.workspaceLeft - 1);
  expect(bounds.right).toBeLessThanOrEqual(bounds.workspaceRight + 1);
  await expect(actions.locator("button")).toHaveCount(2);
  await actions.getByRole("button", { name: /More actions for/i }).click();
  await expect(page.getByRole("menuitem", { name: "Edit notes" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Delete" })).toBeVisible();
});
