import {
    Box,
    Button,
    Center,
    Divider,
    FormControl,
    FormLabel,
    HStack,
    Radio,
    RadioGroup,
    Select,
    Text,
    Tooltip,
    useToast,
    VStack,
} from "@chakra-ui/react";
import Debug from "debug";
import React, { Dispatch } from "react";
import { AsyncState, useAsync } from "react-async";
import { useHistory } from "react-router-dom";
import { Action, BorrowState, Rate } from "./App";
import { getLoanOffer, LoanError, postLoanFinalization, postLoanRequest } from "./Bobtimus";
import NumberInput from "./components/NumberInput";
import WavesProvider from "./waves-provider";
import { Status, WalletStatus } from "./waves-provider/wavesProvider";

const debug = Debug("Borrow");
const error = Debug("Borrow:error");

interface BorrowProps {
    dispatch: Dispatch<Action>;
    rate: Rate;
    state: BorrowState;
    walletStatusAsyncState: AsyncState<WalletStatus>;
    wavesProvider: WavesProvider | undefined;
}

function Borrow({ dispatch, state, rate, wavesProvider, walletStatusAsyncState }: BorrowProps) {
    const toast = useToast();
    const history = useHistory();

    const loanOfferHook = useAsync({
        promiseFn: getLoanOffer,
        onResolve: (data) => {
            dispatch({
                type: "UpdateLoanOffer",
                value: data,
            });
        },
    });
    let { isLoading: loanOfferLoading, data: loanOffer } = loanOfferHook;
    // TODO: Some mechanism to refresh the loan offer values
    //  Note: The rate is currently not taken from the offer, but from the state's rate

    let { data: walletStatus, reload: reloadWalletStatus, error: walletStatusError } = walletStatusAsyncState;

    function onPrincipalAmountChange(newAmount: string) {
        dispatch({
            type: "UpdatePrincipalAmount",
            value: newAmount,
        });
    }

    const interestRate = loanOffer ? loanOffer.base_interest_rate : 0;
    const minPrincipal = loanOffer ? loanOffer.min_principal : 0;
    const maxPrincipal = loanOffer ? loanOffer.max_principal : 0;

    const principalAmount = Number.parseFloat(state.principalAmount);

    // TODO: Let the user define the collateral amount that is within Bobtimus' LTV bounds
    let interestAmount = principalAmount * interestRate;
    let repaymentAmount = principalAmount + interestAmount;

    // The bid price is used so the lender is covered under the assumption of selling the asset
    let collateralAmount = (repaymentAmount * state.collateralization) / rate.bid;

    let terms = loanOffer ? loanOffer.terms : [];
    let collateralizations = loanOffer ? loanOffer.collateralizations : [];

    const maxLTV = loanOffer ? loanOffer.max_ltv : 0;

    let { run: takeLoan, isLoading: isTakingLoan } = useAsync({
        deferFn: async () => {
            if (!wavesProvider) {
                error("Cannot borrow. Waves provider not found.");
                return;
            }

            try {
                const feeRate = state.loanOffer!.fee_sats_per_vbyte;

                let loanRequestWalletParams = await wavesProvider.makeLoanRequestPayload(
                    collateralAmount.toString(),
                    feeRate.toString(),
                );

                let loanResponse = await postLoanRequest(
                    loanRequestWalletParams,
                    state.loanTermInDays,
                    state.collateralization,
                    principalAmount,
                );
                debug(JSON.stringify(loanResponse));

                let loanTransaction = await wavesProvider.signLoan(loanResponse);
                let txid = await postLoanFinalization(loanTransaction);

                // TODO: Add different page for loaned?
                history.push(`/trade/swapped/${txid}`);
            } catch (e) {
                if (e instanceof LoanError) {
                    const description = e.description ? e.description : "";

                    toast({
                        title: e.title,
                        description,
                        status: "error",
                        duration: 10000,
                        isClosable: true,
                    });
                } else {
                    const description = typeof e === "string" ? e : JSON.stringify(e);

                    toast({
                        title: "Error",
                        description,
                        status: "error",
                        duration: 5000,
                        isClosable: true,
                    });
                }
            }
        },
    });

    async function get_extension() {
        // TODO forward to firefox app store
        debug("Download our awesome extension from...");
        reloadWalletStatus();
    }

    async function unlock_wallet() {
        // TODO send request to open popup to unlock wallet
        debug("For now open popup manually...");
        reloadWalletStatus();
    }

    let loanButton;
    if (!wavesProvider || walletStatusError) {
        if (walletStatusError) {
            error(walletStatusError);
        }
        loanButton = <Button
            onClick={async () => {
                await get_extension();
            }}
            variant="primary"
            w="15rem"
            data-cy="get-extension-button"
        >
            Get Extension
        </Button>;
    } else {
        switch (walletStatus?.status) {
            case Status.None:
            case Status.NotLoaded:
                loanButton = <Button
                    onClick={async () => {
                        await unlock_wallet();
                    }}
                    variant="primary"
                    w="15rem"
                    data-cy="unlock-wallet-button"
                >
                    Unlock Wallet
                </Button>;
                break;
            case Status.Loaded:
                loanButton = <Button
                    variant="primary"
                    w="15rem"
                    isLoading={isTakingLoan}
                    onClick={takeLoan}
                    data-cy="data-cy-take-loan-button"
                >
                    Take loan
                </Button>;
                break;
        }
    }

    const w = "60%";
    const w2 = "40%";

    return (
        <VStack spacing={4} align="stretch">
            <VStack padding={4} spacing={4} align="stretch" bg="gray.100" borderRadius={"md"}>
                <HStack>
                    <Box borderRadius={"md"} w={w}>
                        <FormControl id="principalAmount" isRequired>
                            <FormLabel>Desired amount</FormLabel>
                            <Tooltip
                                label={"min = " + minPrincipal + " max = " + maxPrincipal}
                                aria-label="principal"
                                hasArrow
                                placement={"right"}
                            >
                                <Box>
                                    <NumberInput
                                        currency="$"
                                        value={state.principalAmount}
                                        precision={2}
                                        step={0.01}
                                        onAmountChange={onPrincipalAmountChange}
                                        isDisabled={loanOfferLoading}
                                        dataCy={"data-cy-principal"}
                                    />
                                </Box>
                            </Tooltip>
                            {/*<FormHelperText>You will receive this amount into your wallet.</FormHelperText>*/}
                        </FormControl>
                    </Box>
                    <Box borderRadius={"md"} w={w2}>
                        <VStack>
                            <Text align={"left"} fontWeight="bold">Interest rate</Text>
                            <Text align={"left"}>{interestRate * 100}%</Text>
                        </VStack>
                    </Box>
                </HStack>

                <Divider />

                <HStack>
                    <Box borderRadius={"md"} w={w}>
                        <FormControl as="fieldset" id="loanTerm" isRequired>
                            <FormLabel as="legend">Desired term (in days)</FormLabel>
                            <RadioGroup
                                defaultValue="30"
                                onChange={(data) => {
                                    debug("term selected " + data);
                                    dispatch({
                                        type: "UpdateLoanTerm",
                                        value: Number.parseFloat(data.toString()),
                                    });
                                }}
                            >
                                <HStack spacing="24px">
                                    <Radio value="30">30</Radio>
                                    <Radio value="90">90</Radio>
                                    <Radio value="180">180</Radio>
                                </HStack>
                            </RadioGroup>
                            {/*<FormHelperText>For how long do you need the loan.</FormHelperText>*/}
                        </FormControl>
                    </Box>
                    <Box borderRadius={"md"} w={w2}>
                        <VStack>
                            <Text align={"left"} fontWeight="bold">Repayment amount</Text>
                            <Text align={"left"}>{repaymentAmount} USDT</Text>
                        </VStack>
                    </Box>
                </HStack>

                <Divider />

                <HStack>
                    <Box borderRadius={"md"} w={w}>
                        <FormControl id="ltvRatio" required>
                            <FormLabel>Loan-To-Value Ratio (LTV)</FormLabel>
                            <Select defaultValue="50%" bg="white">
                                <option>75% LTV</option>
                                <option>50% LTV</option>
                                <option>25% LTV</option>
                            </Select>
                            {/*<FormHelperText>*/}
                            {/*    Determines the amount of collateral you need to take out the loan.*/}
                            {/*</FormHelperText>*/}
                        </FormControl>
                    </Box>
                    <Box borderRadius={"md"} w={w2}>
                        <VStack>
                            <Text align={"left"} fontWeight="bold">Required collateral</Text>
                            <Text align={"left"}>{Math.floor(collateralAmount * 100000) / 10000} BTC</Text>
                        </VStack>
                    </Box>
                </HStack>

                <Divider />

                <HStack>
                    <Box borderRadius={"md"} w="30%">
                        <VStack>
                            <Text align={"left"} fontWeight="bold">Max LTV</Text>
                            <Text align={"left"} color="red.500">{maxLTV * 100}%</Text>
                        </VStack>
                    </Box>
                    <Box borderRadius={"md"} w="30%">
                        <VStack>
                            <Text align={"left"} fontWeight="bold">Current rate</Text>
                            <Text align={"left"}>{rate.bid}</Text>
                        </VStack>
                    </Box>
                    <Box borderRadius={"md"} w="30%">
                        <VStack>
                            <Text align={"left"} fontWeight="bold">Liquidation rate</Text>
                            <Text align={"left"} color="red.500">{rate.bid}</Text>
                        </VStack>
                    </Box>
                </HStack>
            </VStack>

            <Center>
                {loanButton}
            </Center>
        </VStack>
    );
}

export default Borrow;
