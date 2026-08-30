import { expect, test } from "@playwright/test";

// The episode surface has to be reachable in a running app, not only in a unit test that renders
// the panel directly. Version 0.8.0 shipped it hidden behind an existing cast, which is exactly
// backwards for someone looking for where a multi-character episode is authored.
test("the Cast tab is offered before an episode has a cast", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Video Studio/i }).first().click();
  await page
    .getByRole("main")
    .getByRole("button", { name: /Creator update · Reel draft/i })
    .first()
    .click();

  const castTab = page.getByRole("tab", { name: "Cast" });
  await expect(castTab).toBeVisible({ timeout: 30_000 });
  await castTab.click();
  // An episode with no script says so and points at how to get one.
  await expect(page.getByText(/no cast yet/i)).toBeVisible();
  await expect(page.getByText(/multi-character script/i)).toBeVisible();
});
