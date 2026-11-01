use super::Bucket;
impl Bucket {
    pub fn cost_explanation(&self) -> &'static str {
        if self.coverage.unknown {
            if self.coverage.has_known() {
                "Some usage could not be priced and is excluded. This is the known portion of usage cost, not an invoice."
            } else {
                "No usable cost record or verified model rate and token breakdown is available."
            }
        } else if self.coverage.estimated {
            "Estimated from model token rates; not a subscription fee or invoice. Service-tier and other unlogged charges are excluded."
        } else {
            "Cost recorded by the tool; it may differ from the actual bill."
        }
    }
}
