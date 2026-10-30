import Link from "next/link";
import { PhoneOutgoingIcon } from "lucide-react";

import { Button } from "@/components/ui/button";

export function NewCallButton() {
  return (
    <Button asChild size="sm">
      <Link href="/calls/new">
        <PhoneOutgoingIcon data-icon="inline-start" />
        New call
      </Link>
    </Button>
  );
}
