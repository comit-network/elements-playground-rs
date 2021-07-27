import { Button, FormControl, FormErrorMessage, Input, InputGroup, InputRightElement } from "@chakra-ui/react";
import Debug from "debug";
import * as React from "react";
import { ChangeEvent, useState } from "react";
import { useAsync } from "react-async";
import { unlockWallet } from "../background-proxy";

Debug.enable("*");
const debug = Debug("unlock-wallet");

type UnlockWalletProps = {
    onUnlock: () => void;
};

function UnlockWallet({ onUnlock }: UnlockWalletProps) {
    const [show, setShow] = React.useState(false);
    const [password, setPassword] = useState("");
    const onPasswordChange = (event: ChangeEvent<HTMLInputElement>) => setPassword(event.target.value);
    const handleClick = () => setShow(!show);

    let { run, isPending, isRejected } = useAsync({
        deferFn: async () => {
            await unlockWallet(password);
            onUnlock();
        },
        onReject: (e) => debug("Failed to unlock wallet: %s", e),
    });

    return (
        <>
            <form
                onSubmit={async e => {
                    e.preventDefault();
                    run();
                }}
            >
                <FormControl id="password" isInvalid={isRejected}>
                    <InputGroup size="md">
                        <Input
                            pr="4.5rem"
                            type={show ? "text" : "password"}
                            placeholder="Enter password"
                            value={password}
                            onChange={onPasswordChange}
                            data-cy={"data-cy-unlock-wallet-password-input"}
                        />
                        <InputRightElement width="4.5rem">
                            <Button
                                h="1.75rem"
                                size="sm"
                                onClick={handleClick}
                                data-cy={"data-cy-unlock-wallet-button"}
                            >
                                {show ? "Hide" : "Show"}
                            </Button>
                        </InputRightElement>
                    </InputGroup>
                    <FormErrorMessage>Failed to unlock wallet. Wrong password?</FormErrorMessage>
                </FormControl>
                <Button
                    type="submit"
                    variant="solid"
                    isLoading={isPending}
                    data-cy={"data-cy-unlock-wallet-button"}
                >
                    {"Unlock"}
                </Button>
            </form>
        </>
    );
}

export default UnlockWallet;
