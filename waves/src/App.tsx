import { ExternalLinkIcon } from "@chakra-ui/icons";
import { Box, Button, Center, Flex, Link, Text, VStack } from "@chakra-ui/react";
import React, { useEffect, useReducer } from "react";
import { BrowserRouter, Link as RouterLink, Route, Switch } from "react-router-dom";
import { RingLoader } from "react-spinners";
import "./App.css";
import AssetSelector from "./components/AssetSelector";
import ExchangeIcon from "./components/ExchangeIcon";
import { useRateService } from "./hooks/RateService";
import SwapWithWallet from "./SwapWithWallet";
import UnlockWallet from "./UnlockWallet";
import WalletBalance from "./WalletBalance";

export enum AssetType {
    BTC = "BTC",
    USDT = "USDT",
}

export type AssetSide = "Alpha" | "Beta";

export type Action =
    | { type: "UpdateAlphaAmount"; value: number }
    | { type: "UpdateAlphaAssetType"; value: AssetType }
    | { type: "UpdateBetaAssetType"; value: AssetType }
    | { type: "UpdateRate"; value: number }
    | { type: "SwapAssetTypes" }
    | { type: "PublishTransaction"; value: string };

interface State {
    alpha: AssetState;
    beta: AssetState;
    rate: number;
    txId: string;
    wallet: Wallet;
}

interface Wallet {
    usdtBalance: number;
    btcBalance: number;
}

interface AssetState {
    type: AssetType;
    amount: number;
}

const initialState = {
    alpha: {
        type: AssetType.BTC,
        amount: 0.01,
    },
    beta: {
        type: AssetType.USDT,
        amount: 191.34,
    },
    rate: 19133.74,
    txId: "",
    wallet: {
        usdtBalance: 0,
        btcBalance: 0,
    },
};

export function reducer(state: State = initialState, action: Action) {
    switch (action.type) {
        case "UpdateAlphaAmount":
            return {
                ...state,
                alpha: {
                    type: state.alpha.type,
                    amount: action.value,
                },
                beta: {
                    type: state.beta.type,
                    amount: action.value * state.rate,
                },
                rate: state.rate,
            };
        case "UpdateAlphaAssetType":
            let beta = state.beta;
            if (beta.type === action.value) {
                beta.type = state.alpha.type;
            }
            return {
                ...state,
                beta,
                alpha: {
                    type: action.value,
                    amount: state.alpha.amount,
                },
            };

        case "UpdateBetaAssetType":
            let alpha = state.alpha;
            if (alpha.type === action.value) {
                alpha.type = state.beta.type;
            }
            return {
                ...state,
                alpha: alpha,
                beta: {
                    type: action.value,
                    amount: state.beta.amount,
                },
            };
        case "UpdateRate":
            // TODO: fix "set USDT to alpha, win!"-bug
            return {
                ...state,
                beta: {
                    ...state.beta,
                    amount: state.alpha.amount * action.value,
                },
                rate: action.value,
            };
        case "SwapAssetTypes":
            return {
                ...state,
                alpha: state.beta,
                beta: state.alpha,
            };
        case "PublishTransaction":
            return {
                ...state,
            };
        default:
            throw new Error("Unknown update action received");
    }
}

function App() {
    const [state, dispatch] = useReducer(reducer, initialState);

    const [txPending, setTxPending] = React.useState(false);

    const onConfirmed = (txId: string) => {
        // TODO temp UI hack to make the button loading :)
        setTxPending(true);
        setTimeout(() => {
            setTxPending(false);
        }, 2000);
    };

    const rateService = useRateService();
    useEffect(() => {
        const subscription = rateService.subscribe((rate) => {
            // setBetaAmount(alphaAmount * rate); TODO update amount accordingly
            dispatch({
                type: "UpdateRate",
                value: rate,
            });
        });
        return () => {
            rateService.unsubscribe(subscription);
        };
    }, [rateService]);

    return (
        <BrowserRouter>
            <div className="App">
                <header className="App-header">
                    <Route path="/swap">
                        <WalletBalance btcBalance={state.wallet.btcBalance} usdtBalance={state.wallet.usdtBalance} />
                    </Route>
                </header>
                <Center className="App-body">
                    <VStack
                        spacing={4}
                        align="stretch"
                    >
                        <Flex color="white">
                            <AssetSelector
                                assetSide="Alpha"
                                placement="left"
                                amount={state.alpha.amount}
                                type={state.alpha.type}
                                dispatch={dispatch}
                            />
                            <Center w="10px">
                                <Box zIndex={2}>
                                    <ExchangeIcon dispatch={dispatch} />
                                </Box>
                            </Center>
                            <AssetSelector
                                assetSide="Beta"
                                placement="right"
                                amount={state.beta.amount}
                                type={state.beta.type}
                                dispatch={dispatch}
                            />
                        </Flex>
                        <Box>
                            <Text textStyle="info">1 BTC = {state.rate} USDT</Text>
                        </Box>
                        <Box>
                            <Switch>
                                <Route exact path="/">
                                    <UnlockWallet />
                                </Route>
                                <Route path="/swap">
                                    <SwapWithWallet
                                        onConfirmed={onConfirmed}
                                        dispatch={dispatch}
                                        alphaAmount={state.alpha.amount}
                                        betaAmount={state.beta.amount}
                                        alphaAsset={state.alpha.type}
                                        betaAsset={state.beta.type}
                                    />
                                </Route>
                                <Route path="/done">
                                    <VStack>
                                        <Text textStyle="info">
                                            Check in <Link
                                                href={`https://blockstream.info/liquid/tx/${state.txId}`}
                                                isExternal
                                            >
                                                Block Explorer <ExternalLinkIcon mx="2px" />
                                            </Link>
                                        </Text>
                                        <Button
                                            isLoading={txPending}
                                            size="lg"
                                            variant="main_button"
                                            spinner={<RingLoader size={50} color="white" />}
                                            as={RouterLink}
                                            to="/swap"
                                        >
                                            Swap again?
                                        </Button>
                                    </VStack>
                                </Route>
                            </Switch>
                        </Box>
                    </VStack>
                </Center>
            </div>
        </BrowserRouter>
    );
}

export default App;
