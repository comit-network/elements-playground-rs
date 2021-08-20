import { Box, Button, Center, Divider, HStack, Image, useToast, VStack } from "@chakra-ui/react";
import Debug from "debug";
import React, { useEffect, useReducer } from "react";
import { useAsync } from "react-async";
import { useSSE } from "react-hooks-sse";
import { Link as RouteLink, Redirect, Route, Switch, useHistory } from "react-router-dom";
import "./App.css";
import { fundAddress, LoanOffer } from "./Bobtimus";
import Borrow from "./Borrow";
import COMIT from "./components/comit_logo_spellout_opacity_50.svg";
import Trade from "./Trade";

Debug.enable("*");
const debug = Debug("App");
const error = Debug("App:error");

export enum Asset {
    LBTC = "L-BTC",
    USDT = "USDt",
}

export type AssetSide = "Alpha" | "Beta";

export type Action =
    | { type: "UpdateAlphaAmount"; value: string }
    | { type: "UpdateAlphaAssetType"; value: Asset }
    | { type: "UpdateBetaAssetType"; value: Asset }
    | {
        type: "SwapAssetTypes";
        value: {
            betaAmount: number;
        };
    }
    | { type: "PublishTransaction"; value: string }
    | { type: "UpdateWalletStatus"; value: WalletStatus }
    | { type: "UpdateBalance"; value: Balances }
    | { type: "UpdatePrincipalAmount"; value: string }
    | { type: "UpdateLoanTerm"; value: number }
    | { type: "UpdateLoanOffer"; value: LoanOffer };

export interface TradeState {
    alpha: AssetState;
    beta: Asset;
    txId: string;
    rate: Rate;
}

export interface BorrowState {
    // user can select
    loanTermInDays: number;
    principalAmount: string;
    ltv: number;

    // from Bobtimus
    loanOffer: LoanOffer | null;
}

export interface State {
    trade: TradeState;
    borrow: BorrowState;
    wallet: Wallet;
}

export interface Rate {
    ask: number;
    bid: number;
}

interface Wallet {
    status: WalletStatus;
    balance: Balance;
}

interface Balance {
    usdtBalance: number;
    btcBalance: number;
}

interface WalletStatus {
    exists: boolean;
    loaded: boolean;
}

export interface Balances {
    usdt: number;
    btc: number;
}

interface AssetState {
    type: Asset;
    amount: string;
}

const initialState: State = {
    trade: {
        alpha: {
            type: Asset.LBTC,
            amount: "0.01",
        },
        beta: Asset.USDT,
        // TODO: These default values are shown briefly after every
        // refresh
        rate: {
            ask: 20000.0,
            bid: 19000.0,
        },
        txId: "",
    },
    borrow: {
        principalAmount: "0.0",
        loanTermInDays: 0,
        loanOffer: null,
        ltv: 0.5,
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

export function reducer(state: State = initialState, action: Action) {
    switch (action.type) {
        case "UpdateAlphaAmount":
            return {
                ...state,
                trade: {
                    ...state.trade,
                    alpha: {
                        type: state.trade.alpha.type,
                        amount: action.value,
                    },
                },
            };
        case "UpdateAlphaAssetType":
            let beta = state.trade.beta;
            if (beta === action.value) {
                beta = state.trade.alpha.type;
            }
            return {
                ...state,
                trade: {
                    ...state.trade,
                    beta,
                    alpha: {
                        type: action.value,
                        amount: state.trade.alpha.amount,
                    },
                },
            };

        case "UpdateBetaAssetType":
            let alpha = state.trade.alpha;
            if (alpha.type === action.value) {
                alpha.type = state.trade.beta;
            }
            return {
                ...state,
                trade: {
                    ...state.trade,
                    alpha,
                    beta: action.value,
                },
            };
        case "SwapAssetTypes":
            return {
                ...state,
                trade: {
                    ...state.trade,
                    alpha: {
                        type: state.trade.beta,
                        amount: state.trade.alpha.amount,
                    },
                    beta: state.trade.alpha.type,
                },
            };
        case "PublishTransaction":
            return {
                ...state,
            };
        case "UpdateBalance":
            return {
                ...state,
                wallet: {
                    ...state.wallet,
                    balance: {
                        usdtBalance: action.value.usdt,
                        btcBalance: action.value.btc,
                    },
                },
            };
        case "UpdateWalletStatus":
            return {
                ...state,
                wallet: {
                    ...state.wallet,
                    status: {
                        exists: action.value.exists,
                        loaded: action.value.loaded,
                    },
                },
            };

        case "UpdatePrincipalAmount":
            return {
                ...state,
                borrow: {
                    ...state.borrow,
                    principalAmount: action.value,
                },
            };
        case "UpdateLoanTerm":
            return {
                ...state,
                borrow: {
                    ...state.borrow,
                    loanTerm: action.value,
                },
            };
        case "UpdateLoanOffer":
            // TODO: We currently always overwrite upon a new loan offer
            //  This will have to be adapted once we refresh loan offers.
            const principalAmount = action.value.min_principal.toString();
            const loanTermInDays = action.value.terms[0].days;
            debug("%s", JSON.stringify(action.value));
            const ltv = action.value.initial_ltvs[0].ltv;

            return {
                ...state,
                borrow: {
                    ...state.borrow,
                    principalAmount,
                    loanTermInDays,
                    ltv,
                    loanOffer: action.value,
                },
            };
        default:
            throw new Error("Unknown update action received");
    }
}

const wavesProvider = window.wavesProvider;

function walletStatus() {
    if (!wavesProvider) {
        throw new Error("No extension");
    }

    return wavesProvider.walletStatus();
}

function App() {
    const history = useHistory();
    const toast = useToast();
    const path = history.location.pathname;

    useEffect(() => {
        if (path === "/app") {
            history.replace("/");
        }
    }, [path, history]);

    const [state, dispatch] = useReducer(reducer, initialState);

    const rate = useSSE("rate", {
        ask: 33766.30,
        bid: 33670.10,
    });

    let walletStatusAsyncState = useAsync({
        promiseFn: walletStatus,
    });

    let { run: callFaucet, isLoading: isFaucetLoading } = useAsync({
        deferFn: async () => {
            try {
                if (wavesProvider) {
                    let address = await wavesProvider.getNewAddress();
                    await fundAddress(address);
                } else {
                    debug("Waves provider undefined. Cannot call faucet without an address");
                }
            } catch (e) {
                error("Could not call faucet: {}", e);
                toast({
                    title: "Error",
                    description: `Could not call faucet: ${e}`,
                    status: "error",
                    duration: 9000,
                    isClosable: true,
                });
            }
        },
    });

    return (
        <Box className="App">
            <Route exact path="/">
                <Redirect to="/trade" />
            </Route>

            <header className="App-header">
                <HStack align="left">
                    <Button variant="secondary" onClick={callFaucet} isLoading={isFaucetLoading}>Faucet</Button>
                </HStack>
                <Center>
                    <Image src={COMIT} h="24px" />
                </Center>
            </header>

            <Center className="App-body">
                <VStack spacing={4}>
                    <HStack spacing={4} as="nav">
                        <NavLink text="Trade" path={"/trade"} />
                        <NavLink text="Borrow" path={"/borrow"} />
                    </HStack>
                    <Divider />
                    <Switch>
                        <Route path="/trade">
                            <Trade
                                state={state.trade}
                                dispatch={dispatch}
                                rate={rate}
                                walletStatusAsyncState={walletStatusAsyncState}
                                wavesProvider={wavesProvider}
                            />
                        </Route>
                        <Route path="/borrow">
                            <Borrow
                                dispatch={dispatch}
                                state={state.borrow}
                                rate={rate}
                                walletStatusAsyncState={walletStatusAsyncState}
                                wavesProvider={wavesProvider}
                            />
                        </Route>
                    </Switch>
                </VStack>
            </Center>
        </Box>
    );
}

type NavLinkProps = { text: string; path: string };
const NavLink = ({ text, path }: NavLinkProps) => (
    <RouteLink to={path}>
        <Route
            path={path}
            children={({ match }) => (
                <Button width="100px" colorScheme="blue" variant={match?.path ? "solid" : "outline"}>
                    {text}
                </Button>
            )}
        />
    </RouteLink>
);

export default App;
