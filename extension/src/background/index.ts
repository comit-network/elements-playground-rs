import Debug from "debug";
import { browser, Windows } from "webextension-polyfill-ts";
import WavesProvider from "../in-page";
import { BackupDetails, LoanDetails, LoanToSign, SwapToSign, Txid } from "../models";
import {
    bip39SeedWords,
    createLoanBackup,
    createNewBip39Wallet,
    extractLoan,
    extractTrade,
    getAddress,
    getBalances,
    getOpenLoans,
    getPastTransactions,
    loadLoanBackup,
    makeBuyCreateSwapPayload,
    makeLoanRequestPayload,
    makeSellCreateSwapPayload,
    repayLoan,
    signAndSendSwap,
    signLoan,
    unlockWallet,
    walletStatus,
    withdrawAll,
} from "../wasmProxy";

// TODO: Is this global or do we need one per file?
Debug.enable("*");
const debug = Debug("background");

// First thing we load settings
loadSettings();

debug("Hello world from background script");

const walletName = "demo";

var swapToSign: SwapToSign | null;
var resolveSwapSignRequest: ((txid: Txid) => void) | null;
var rejectSwapSignRequest: ((e: any) => void) | null;

var loanToSign: LoanToSign | null;
var resolveLoanSignRequest: ((txid: Txid) => void) | null;
var rejectLoanSignRequest: ((e: any) => void) | null;

var popup: Windows.Window | null;

export interface RpcMessage<T extends keyof WavesProvider> {
    type: "rpc-message";
    method: T;
    args: Parameters<WavesProvider[T]>;
}

/*
 * Defines the public interface of the background script.
 *
 * To ensure maximum benefit from the type checker, other components like the content script should only use this function to send messages.
 */
export function invokeBackgroundScriptRpc(message: Omit<RpcMessage<keyof WavesProvider>, "type">): Promise<any> {
    return browser.runtime.sendMessage({
        type: "rpc-message",
        ...message,
    });
}

addRpcMessageListener("walletStatus", () => walletStatus(walletName));
addRpcMessageListener("getBuyCreateSwapPayload", ([usdt]) => makeBuyCreateSwapPayload(walletName, usdt));
addRpcMessageListener("getSellCreateSwapPayload", ([btc]) => makeSellCreateSwapPayload(walletName, btc));
addRpcMessageListener("getNewAddress", () => getAddress(walletName));
addRpcMessageListener(
    "makeLoanRequestPayload",
    ([collateral, feerate]) => makeLoanRequestPayload(walletName, collateral, feerate),
);

addRpcMessageListener("signAndSendSwap", ([txHex]) => {
    return new Promise((resolve, reject) => {
        extractTrade(walletName, txHex)
            .then(decoded => {
                swapToSign = { txHex, decoded };
                resolveSwapSignRequest = resolve;
                rejectSwapSignRequest = reject;

                updateBadge();
                openPopup();
            })
            .catch(e => {
                reject(e);
                cleanupPendingSwap();
            });
    });
});
addRpcMessageListener("signLoan", ([loanRequest]) => {
    return new Promise((resolve, reject) => {
        extractLoan(walletName, loanRequest)
            .then(details => {
                loanToSign = { details };
                resolveLoanSignRequest = resolve;
                rejectLoanSignRequest = reject;

                updateBadge();
                openPopup();
            })
            .catch(e => {
                reject(e);
                cleanupPendingLoan();
            });
    });
});

async function openPopup() {
    popup = await browser.windows.create({
        focused: true,
        width: 400,
        height: 600,
        type: "popup",
        url: "popup.html",
    });
}

function addRpcMessageListener<T extends keyof WavesProvider>(
    method: T,
    callback: (args: Parameters<WavesProvider[T]>) => ReturnType<WavesProvider[T]>,
) {
    browser.runtime.onMessage.addListener((msg: RpcMessage<T>) => {
        if (msg.type !== "rpc-message" || msg.method !== method) {
            return;
        }

        debug(`Received: %o`, msg);

        return callback(msg.args);
    });
}

// @ts-ignore
window.getWalletStatus = async () => {
    return walletStatus(walletName);
};
// @ts-ignore
window.unlockWallet = async (password: string) => {
    return unlockWallet(walletName, password);
};
// @ts-ignore
window.getBalances = async () => {
    return getBalances(walletName);
};
// @ts-ignore
window.getAddress = async () => {
    return getAddress(walletName);
};
// @ts-ignore
window.getSwapToSign = async () => {
    return swapToSign;
};
// @ts-ignore
window.signAndSendSwap = (txHex: string) => {
    if (!resolveSwapSignRequest || !rejectSwapSignRequest) {
        throw new Error("No pending promise functions for swap sign request");
    }

    signAndSendSwap(walletName, txHex)
        .then(resolveSwapSignRequest)
        .catch(rejectSwapSignRequest)
        .then(cleanupPendingSwap);
};
// @ts-ignore
window.rejectSwap = () => {
    if (!resolveSwapSignRequest || !rejectSwapSignRequest) {
        throw new Error("No pending promise functions for swap sign request");
    }

    rejectSwapSignRequest("User declined signing request");
    cleanupPendingSwap();
};
// @ts-ignore
window.getLoanToSign = () => {
    return loanToSign;
};
// @ts-ignore
window.signLoan = async () => {
    if (!resolveLoanSignRequest || !rejectLoanSignRequest) {
        throw new Error("No pending promise functions for loan sign request");
    }

    // TODO: Currently, we assume that whatever the user has verified
    // on the pop-up matches what is stored in the extension's
    // storage. It would be better to send around the swap ID to check
    // that the wallet is signing the same transaction the user has authorised

    // if we receive an error, we respond directly, else we return the details
    return await signLoan(walletName).catch(rejectLoanSignRequest);
};

// @ts-ignore
window.confirmLoan = async (payload: string) => {
    if (!resolveLoanSignRequest || !rejectLoanSignRequest) {
        throw new Error("No pending promise functions for loan sign request");
    }
    // once sent to the page, we assume the business is done.
    // TODO: a feedback loop is required where the wallet gets told if bobtimus successfully published the transaction
    resolveLoanSignRequest(payload);
    await cleanupPendingLoan();
};

// @ts-ignore
window.createLoanBackup = async (loanTx: string) => {
    return createLoanBackup(walletName, loanTx);
};

// @ts-ignore
window.loadLoanBackup = async (backupDetails: BackupDetails) => {
    return loadLoanBackup(backupDetails);
};

// @ts-ignore
window.rejectLoan = () => {
    if (!resolveLoanSignRequest || !rejectLoanSignRequest) {
        throw new Error("No pending promise functions for loan sign request");
    }

    rejectLoanSignRequest("User declined signing request");
    cleanupPendingLoan();
};
// @ts-ignore
window.withdrawAll = async (address: string) => {
    return withdrawAll(walletName, address);
};
// @ts-ignore
window.getOpenLoans = async (): LoanDetails[] => {
    return getOpenLoans();
};
// @ts-ignore
window.repayLoan = async (txid: string): void => {
    return repayLoan(walletName, txid);
};
// @ts-ignore
window.getPastTransactions = async (): Txid[] => {
    return getPastTransactions(walletName);
};
// @ts-ignore
window.bip39SeedWords = async (): string => {
    return bip39SeedWords();
};
// @ts-ignore
window.createWalletFromBip39 = async (seed_words: string, password: string) => {
    return createNewBip39Wallet(walletName, seed_words, password);
};

function updateBadge() {
    let count = 0;
    if (loanToSign) count++;
    if (swapToSign) count++;
    browser.browserAction.setBadgeText(
        { text: (count === 0 ? null : count.toString()) },
    );
}

function loadSettings() {
    debug("Loading settings");
    ensureVarSet("ESPLORA_API_URL");
    ensureVarSet("CHAIN");
    ensureVarSet("LBTC_ASSET_ID");
    ensureVarSet("LUSDT_ASSET_ID");
}

function cleanupPendingSwap() {
    resolveSwapSignRequest = null;
    rejectSwapSignRequest = null;
    swapToSign = null;
    updateBadge();
    closePopup();
}

function cleanupPendingLoan() {
    resolveLoanSignRequest = null;
    rejectLoanSignRequest = null;
    loanToSign = null;
    updateBadge();

    closePopup();
}

function closePopup() {
    if (popup && popup.id) {
        browser.windows.remove(popup.id);
        popup = null;
    }
}

// First we check environment variable. If set, we honor it and overwrite settings in local storage.
// For the environment variable we add the prefix `REACT_APP_`.
// Else we check browser storage. If set, we honor it, if not, we throw an error as we cannot work
// without this value.
function ensureVarSet(name: string) {
    const uppercase = name.toUpperCase();
    const value = process.env[`REACT_APP_${uppercase}`];
    if (value) {
        debug(`Environment variable provided, overwriting storage: ${name}:${value}`);
        localStorage.setItem(name, value);
    }
}
