import React from "react";
import { Asset, reducer, State } from "./App";
import { LoanOffer } from "./Bobtimus";
import calculateBetaAmount from "./calculateBetaAmount";

const defaultLoanOffer: LoanOffer = {
    rate: {
        ask: 20000,
        bid: 20000,
    },
    fee_sats_per_vbyte: 1,
    min_principal: 100,
    max_principal: 10000,
    max_ltv: 0.8,
    base_interest_rate: 0.15,
    terms: [{
        days: 30,
        interest_mod: 0.01,
    }],
    initial_ltvs: [{
        ltv: 0.5,
    }],
};

const defaultState: State = {
    trade: {
        alpha: {
            type: Asset.LBTC,
            amount: "0",
        },
        beta: Asset.USDT,
        txId: "",
        rate: {
            ask: 0,
            bid: 0,
        },
    },
    borrow: {
        principalAmount: "1000",
        loanTermInDays: 43200, // 30 days in min
        ltv: 0.5,

        loanOffer: defaultLoanOffer,
    },
    wallet: {
        balance: {
            usdtBalance: 0,
            btcBalance: 0,
        },
        status: {
            exists: false,
            loaded: false,
        },
    },
};

test("update alpha amount logic", () => {
    const initialState = {
        ...defaultState,
        trade: {
            ...defaultState.trade,
            alpha: {
                type: Asset.LBTC,
                amount: "0.01",
            },
            beta: Asset.USDT,
        },
    };

    let newValue = "42";
    expect(
        reducer(initialState, {
            type: "UpdateAlphaAmount",
            value: newValue,
        }).trade.alpha.amount,
    ).toBe(newValue);
});

test("update alpha asset logic - should flip asset types", () => {
    const initialState = {
        ...defaultState,
        trade: {
            ...defaultState.trade,
            alpha: {
                type: Asset.LBTC,
                amount: "0.01",
            },
            beta: Asset.USDT,
        },
    };

    let newValue = Asset.USDT;
    expect(
        reducer(initialState, {
            type: "UpdateAlphaAssetType",
            value: newValue,
        }).trade.alpha.type,
    ).toBe(newValue);

    expect(
        reducer(initialState, {
            type: "UpdateAlphaAssetType",
            value: newValue,
        }).trade.beta,
    ).toBe(initialState.trade.alpha.type);

    // amounts should be unchanged
    expect(
        reducer(initialState, {
            type: "UpdateAlphaAssetType",
            value: newValue,
        }).trade.alpha.amount,
    ).toBe(initialState.trade.alpha.amount);
});

test("update beta asset logic - should flip asset types", () => {
    const initialState = {
        ...defaultState,
        trade: {
            ...defaultState.trade,
            alpha: {
                type: Asset.LBTC,
                amount: "0.01",
            },
            beta: Asset.USDT,
        },
    };

    let newValue = Asset.LBTC;
    expect(
        reducer(initialState, {
            type: "UpdateBetaAssetType",
            value: newValue,
        }).trade.beta,
    ).toBe(newValue);

    expect(
        reducer(initialState, {
            type: "UpdateBetaAssetType",
            value: newValue,
        }).trade.alpha.type,
    ).toBe(initialState.trade.beta);

    // amounts should be unchanged
    expect(
        reducer(initialState, {
            type: "UpdateBetaAssetType",
            value: newValue,
        }).trade.alpha.amount,
    ).toBe(initialState.trade.alpha.amount);
});

test("Swap asset types", () => {
    const initialState = {
        ...defaultState,
        trade: {
            ...defaultState.trade,
            alpha: {
                type: Asset.LBTC,
                amount: "0.01",
            },
            beta: Asset.USDT,
        },
    };

    const rate = {
        bid: 10,
        ask: 10,
    };
    // This is just showing how the beta amount is calculated in "reality". The actual amounts and rates don't matter
    // in this test.
    let amountAsNumber = Number.parseFloat(initialState.trade.alpha.amount);
    let betaAmount = calculateBetaAmount(initialState.trade.alpha.type, amountAsNumber, rate);

    expect(
        reducer(initialState, {
            type: "SwapAssetTypes",
            value: {
                betaAmount: betaAmount,
            },
        }).trade.alpha.type,
    ).toBe(initialState.trade.beta);

    expect(
        reducer(initialState, {
            type: "SwapAssetTypes",
            value: {
                betaAmount: betaAmount,
            },
        }).trade.beta,
    ).toBe(initialState.trade.alpha.type);

    // amounts should not be flipped.
    expect(
        reducer(initialState, {
            type: "SwapAssetTypes",
            value: {
                betaAmount: betaAmount,
            },
        }).trade.alpha.amount,
    ).toBe(
        initialState.trade.alpha.amount.toString(),
    );
});

test("update principal amount logic", () => {
    const initialState = {
        ...defaultState,
        borrow: {
            ...defaultState.borrow,
            principalAmount: "10000",
        },
    };

    let newValue = "42";
    expect(
        reducer(initialState, {
            type: "UpdatePrincipalAmount",
            value: newValue,
        }).borrow.principalAmount,
    ).toBe(newValue);
});

test("update loan offer logic", () => {
    const initialState = {
        ...defaultState,
        borrow: {
            loanTermInDays: 0,
            principalAmount: "0",
            loanOffer: null,
            ltv: 0,
        },
    };

    let newState = reducer(initialState, {
        type: "UpdateLoanOffer",
        value: defaultLoanOffer,
    });

    expect(newState.borrow.loanOffer).toBe(defaultLoanOffer);
    expect(newState.borrow.principalAmount).toBe("100");
    expect(newState.borrow.loanTermInDays).toBe(30);
    expect(newState.borrow.ltv).toBe(0.5);
});
