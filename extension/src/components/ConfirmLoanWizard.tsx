import {
    Alert,
    AlertDescription,
    AlertIcon,
    AlertTitle,
    Box,
    Button,
    Flex,
    Heading,
    Image,
    Spacer,
    Text,
    useInterval,
    VStack,
} from "@chakra-ui/react";
import { Step, Steps, useSteps } from "chakra-ui-steps";
import moment from "moment";
import React, { useState } from "react";
import { useAsync } from "react-async";
import { FiCheck, FiClipboard, FiExternalLink } from "react-icons/all";
import { browser } from "webextension-polyfill-ts";
import { confirmLoan, createLoanBackup, rejectLoan, signLoan } from "../background-proxy";
import { LoanToSign, USDT_TICKER } from "../models";
import YouSwapItem from "./SwapItem";
import Usdt from "./tether.svg";

interface ConfirmLoanWizardProps {
    onCancel: () => void;
    onSuccess: () => void;
    loanToSign: LoanToSign;
}

const ConfirmLoanWizard = ({ onCancel, onSuccess, loanToSign }: ConfirmLoanWizardProps) => {
    const { nextStep, activeStep } = useSteps({
        initialStep: 0,
    });
    let [signedTransaction, setSignedTransaction] = useState("");

    let { isPending: isSigning, run: sign } = useAsync({
        deferFn: async () => {
            let signedTransaction = await signLoan();
            setSignedTransaction(signedTransaction);
            nextStep();
        },
    });

    const { isPending: isDownloading, run: downloadLoanBackup } = useAsync({
        deferFn: async () => {
            const loanBackup = await createLoanBackup(signedTransaction);
            const file = new Blob([JSON.stringify(loanBackup)], { type: "text/json" });
            const url = URL.createObjectURL(file);
            await browser.downloads.download({ url, filename: "loan-backup.json" });
            nextStep();
        },
    });

    const { isPending: isPublishing, run: publish } = useAsync({
        deferFn: async () => {
            await confirmLoan(signedTransaction);
            onSuccess();
        },
    });

    return (
        <VStack width="100%">
            <Steps activeStep={activeStep}>
                <Step label={"Sign"} key={"sign"} icon={FiClipboard}>
                    <Flex py={4}>
                        <form
                            onSubmit={async e => {
                                e.preventDefault();
                                sign();
                            }}
                            data-cy="confirm-loan-form"
                        >
                            <ConfirmLoan
                                loanToSign={loanToSign}
                            />
                            <Button
                                variant="secondary"
                                mr={3}
                                onClick={async () => {
                                    await rejectLoan();
                                    onCancel();
                                }}
                            >
                                Cancel
                            </Button>
                            <Button
                                type="submit"
                                variant="primary"
                                isLoading={isSigning}
                                data-cy="data-cy-sign-loan-button"
                            >
                                Sign
                            </Button>
                        </form>
                    </Flex>
                </Step>
                <Step label={"Backup"} key={"backup"} icon={FiExternalLink}>
                    <Flex py={4}>
                        <VStack>
                            <form
                                onSubmit={async e => {
                                    e.preventDefault();
                                    downloadLoanBackup();
                                }}
                                data-cy="confirm-loan-form"
                            >
                                <Alert
                                    status="warning"
                                    variant="subtle"
                                    flexDirection="column"
                                    alignItems="center"
                                    justifyContent="center"
                                    textAlign="center"
                                    height="250px"
                                >
                                    <AlertIcon boxSize="40px" mr={0} />
                                    <AlertTitle mt={4} mb={1} fontSize="lg">
                                        Create a backup!
                                    </AlertTitle>
                                    <AlertDescription>
                                        Click below to download a backup of the loan details. Together with your seed
                                        words you can recover your loan details in case your browser storage gets
                                        purged. Keep it safe! You can restore your backup through the settings.
                                    </AlertDescription>
                                </Alert>
                                <Button
                                    type="submit"
                                    variant="primary"
                                    isLoading={isDownloading}
                                    data-cy="data-cy-download-loan-button"
                                >
                                    Download backup
                                </Button>
                            </form>
                        </VStack>
                    </Flex>
                </Step>

                <Step label={"Confirm"} key={"confirm"} icon={FiCheck}>
                    <Flex py={4}>
                        <form
                            onSubmit={async e => {
                                e.preventDefault();
                                publish();
                            }}
                            data-cy="confirm-loan-form"
                        >
                            <VStack>
                                <Alert
                                    status="info"
                                    variant="subtle"
                                    flexDirection="column"
                                    alignItems="center"
                                    justifyContent="center"
                                    textAlign="center"
                                    height="200px"
                                >
                                    <AlertIcon boxSize="40px" mr={0} />
                                    <AlertTitle mt={4} mb={1} fontSize="lg">
                                        Backup saved?
                                    </AlertTitle>
                                    <AlertDescription>
                                        The lender will publish the transaction once signed.
                                    </AlertDescription>
                                </Alert>
                                <Button
                                    type="submit"
                                    variant="primary"
                                    isLoading={isPublishing}
                                    data-cy="data-cy-confirm-loan-button"
                                >
                                    Confirm
                                </Button>
                            </VStack>
                        </form>
                    </Flex>
                </Step>
            </Steps>
        </VStack>
    );
};

export default ConfirmLoanWizard;

interface ConfirmLoanProps {
    loanToSign: LoanToSign;
}

function ConfirmLoan(
    { loanToSign }: ConfirmLoanProps,
) {
    let { details: { collateral, principal, principalRepayment, term } } = loanToSign;

    let [timestamp, setTimestamp] = useState(Math.floor(Date.now() / 1000));
    useInterval(() => {
        setTimestamp(Math.floor(Date.now() / 1000));
    }, 6000); // 1 min

    const deadline = timestamp && term
        ? moment().add(Math.abs(timestamp - term), "seconds").fromNow()
        : null;

    return (<Box>
        <Heading>Confirm Loan</Heading>
        <Box>
            <YouSwapItem
                tradeSide={collateral}
                action={"send"}
            />
        </Box>
        <Box>
            <YouSwapItem
                tradeSide={principal}
                action={"receive"}
            />
        </Box>
        <Box w="100%">
            <Flex>
                <Box h="40px" p="1">
                    <Text>Repayment amount: {principalRepayment}</Text>
                </Box>
                <Spacer />
                <Box w="40px" h="40px">
                    <Image src={Usdt} h="32px" />
                </Box>
                <Box h="40px" justify="right" p="1">
                    <Text align="center" justify="center">
                        {USDT_TICKER}
                    </Text>
                </Box>
            </Flex>
        </Box>
        <Box w="100%">
            <Flex>
                <Box h="40px" p="1">
                    <Text>Loan term: {term} timestamp {deadline ? "(due " + deadline + ")" : ""}</Text>
                </Box>
            </Flex>
        </Box>
    </Box>);
}
