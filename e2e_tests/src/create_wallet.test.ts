import * as assert from "assert";
import Debug from "debug";
import fetch from "node-fetch";
import { Builder, By, until } from "selenium-webdriver";

const firefox = require("selenium-webdriver/firefox");
const firefoxPath = require("geckodriver").path;

const getElementById = async (driver, xpath, timeout = 4000) => {
    const el = await driver.wait(until.elementLocated(By.xpath(xpath)), timeout);
    return await driver.wait(until.elementIsVisible(el), timeout);
};

describe("webdriver", () => {
    const webAppUrl = "http://localhost:3030";

    let driver;
    let extensionId: string;
    let webAppTitle: string;
    let extensionTitle: string;

    beforeAll(async () => {
        const service = new firefox.ServiceBuilder(firefoxPath);
        const options = new firefox.Options();

        driver = new Builder()
            .setFirefoxService(service)
            .forBrowser("firefox")
            .setFirefoxOptions(options)
            .build();

        await driver.get(webAppUrl);

        await driver.installAddon("../extension/zip/waves_wallet-0.0.1.zip", true);

        // this probably works forever unless we change something and then it won't work anymore
        await driver.get("about:debugging#/runtime/this-firefox");
        const extensionElement = await getElementById(
            driver,
            "//span[contains(text(),'waves_wallet')]//"
                + "parent::li/section/dl/div//dt[contains(text(),'Internal UUID')]/following-sibling::dd",
        );
        extensionId = await extensionElement.getText();

        // load webapp again
        await driver.get(webAppUrl);
        webAppTitle = await driver.getTitle();

        // Opens a new tab and switches to new tab
        await driver.switchTo().newWindow("tab");

        // Open extension
        let extensionUrl = `moz-extension://${extensionId}/popup.html`;
        await driver.get(`${extensionUrl}`);
        extensionTitle = await driver.getTitle();
    }, 20000);

    afterAll(async () => {
        await driver.quit();
    });

    async function getWindowHandle(name: string) {
        let allWindowHandles = await driver.getAllWindowHandles();
        for (const windowHandle of allWindowHandles) {
            await driver.switchTo().window(windowHandle);
            const title = await driver.getTitle();
            if (title === name) {
                return windowHandle;
            }
        }
    }

    async function switchToWindow(name: string) {
        await driver.switchTo().window(await getWindowHandle(name));
    }

    test("Create wallet", async () => {
        const debug = Debug("e2e-create");

        await switchToWindow(extensionTitle);

        debug("Choosing password");

        let step1 = await getElementById(driver, "//button[@data-cy='data-cy-create-wallet-step-1']");
        await step1.click();

        let mnemonic = "bargain pretty shop spy travel toilet hero ridge critic race weapon elbow";

        let mnemonicInput = await getElementById(driver, "//textarea[@data-cy='data-cy-create-wallet-mnemonic-input']");
        await mnemonicInput.sendKeys(mnemonic);

        let checkBox = await getElementById(driver, "//label[@data-cy='data-cy-create-wallet-checkbox-input']");
        await checkBox.click();

        let step2 = await getElementById(driver, "//button[@data-cy='data-cy-create-wallet-step-2']");
        await step2.click();

        let mnemonicConfirmationInput = await getElementById(
            driver,
            "//textarea[@data-cy='data-cy-create-wallet-mnemonic-input-confirmation']",
        );
        await mnemonicConfirmationInput.sendKeys(mnemonic);

        let password = "foo";
        let passwordInput = await getElementById(driver, "//input[@data-cy='data-cy-create-wallet-password-input']");
        await passwordInput.sendKeys(password);

        debug("Creating wallet");
        let createWalletButton = await getElementById(
            driver,
            "//button[@data-cy='data-cy-create-wallet-button']",
        );
        await createWalletButton.click();

        debug("Getting wallet address");
        let addressField = await getElementById(driver, "//p[@data-cy='data-cy-wallet-address-text-field']");
        let address = await addressField.getText();
        debug(`Address found: ${address}`);

        // TODO: re-enable faucet again
        let url = `${webAppUrl}/api/faucet/${address}`;
        debug("Calling faucet: %s", url);
        let response = await fetch(url, {
            method: "POST",
        });
        assert(response.ok);
        let body = await response.text();
        debug("Faucet response: %s", body);

        // TODO: Remove when automatic balance refreshing is
        // implemented
        await new Promise(r => setTimeout(r, 10_000));
        await driver.navigate().refresh();

        debug("Waiting for balance update");
        let btcAmount = await getElementById(driver, "//p[@data-cy='data-cy-L-BTC-balance-text-field']", 20_000);
        debug("Found L-BTC amount: %s", await btcAmount.getText());
    }, 30_000);
});
