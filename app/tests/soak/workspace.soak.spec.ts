import { expect, test } from "@playwright/test";

const routes = ["Generate", "Projects", "Voices", "Models", "Compare", "Benchmarks", "History", "Settings", "About"];
const cycles = Math.max(1, Number.parseInt(process.env.SOUNDAR_SOAK_CYCLES ?? "25", 10) || 25);

test("repeated route and theme changes remain bounded and error-free", async ({ page }) => {
  const browserErrors: string[] = [];
  page.on("pageerror", (error) => browserErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") browserErrors.push(message.text());
  });

  await page.goto("/");
  const sidebar = page.locator(".sidebar");

  for (let cycle = 0; cycle < cycles; cycle += 1) {
    const desiredTheme = cycle % 2 === 0 ? "light" : "dark";
    const themeButton = page.getByRole("button", { name: desiredTheme === "light" ? "Cream light" : "Dark mode" }).filter({ visible: true });
    if (await themeButton.count()) await themeButton.click();

    for (const route of routes) {
      await sidebar.getByRole("button", { name: route, exact: true }).click();
      await expect(page.getByRole("heading", { name: route })).toBeVisible();

      const bounds = await page.locator("main").evaluate((main) => {
        const box = main.getBoundingClientRect();
        const offenders = [...main.querySelectorAll<HTMLElement>("button, input:not([type=range]), textarea, [role=combobox], .panel")]
          .filter((element) => {
            const style = getComputedStyle(element);
            const rect = element.getBoundingClientRect();
            if (style.display === "none" || style.visibility === "hidden" || rect.width === 0 || rect.height === 0) return false;
            return rect.left < box.left - 1 || rect.right > box.right + 1 || element.scrollWidth > element.clientWidth + 2;
          })
          .map((element) => (element.innerText || element.getAttribute("aria-label") || element.tagName).trim().slice(0, 60));
        return {
          documentWidth: document.documentElement.scrollWidth,
          viewportWidth: document.documentElement.clientWidth,
          offenders,
        };
      });
      expect(bounds.documentWidth, `cycle ${cycle + 1}, ${route} widened the workspace`).toBeLessThanOrEqual(bounds.viewportWidth + 1);
      expect(bounds.offenders, `cycle ${cycle + 1}, ${route} clipped controls`).toEqual([]);
    }
  }

  expect(browserErrors).toEqual([]);
});
