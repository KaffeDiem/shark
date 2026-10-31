use super::{Bucket, CostCoverage};
use crate::pokemon::format::Language;
use serde::{Deserialize, Deserializer};
impl Bucket {
    pub fn localized_cost_text(&self, language: Language, compact: bool) -> String {
        if self.coverage.unknown && !self.coverage.has_known() {
            if compact {
                "$—".into()
            } else {
                unavailable(language).into()
            }
        } else {
            self.cost_text(compact)
        }
    }
    pub fn cost_explanation(&self, language: Language) -> &'static str {
        if self.coverage.unknown {
            if self.coverage.has_known() {
                partial_hint(language)
            } else {
                unavailable_hint(language)
            }
        } else if self.coverage.estimated {
            estimate_hint(language)
        } else {
            reported_hint(language)
        }
    }
}
fn unavailable(language: Language) -> &'static str {
    [
        "계산 불가",
        "Unavailable",
        "計算不可",
        "No disponible",
        "Indisponible",
        "Indisponível",
        "Nicht verfügbar",
        "Недоступно",
    ][language as usize]
}
fn estimate_hint(language: Language) -> &'static str {
    [
        "모델 단가로 환산한 추정 비용입니다. 구독료나 실제 청구액이 아닙니다.",
        "Estimated from model token rates; not a subscription fee or invoice. Service-tier and other unlogged charges are excluded.",
        "モデル単価による推定です。購読料や実際の請求額ではありません。",
        "Estimación por tarifas del modelo; no es la cuota ni la factura real.",
        "Estimation selon les tarifs du modèle, pas un abonnement ni une facture.",
        "Estimativa pelas tarifas do modelo; não é assinatura nem fatura.",
        "Schätzung anhand der Modellpreise, keine Abogebühr oder Rechnung.",
        "Оценка по тарифам моделей за токены — не плата за подписку и не счёт. Наценки за уровень обслуживания и другие незаписанные расходы не учтены.",
    ][language as usize]
}
fn reported_hint(language: Language) -> &'static str {
    [
        "도구가 기록한 비용입니다. 실제 청구액과 다를 수 있습니다.",
        "Cost recorded by the tool; it may differ from the actual bill.",
        "ツールが記録したコストです。実際の請求額とは異なる場合があります。",
        "Coste registrado por la herramienta; puede diferir de la factura.",
        "Coût enregistré par l’outil, pouvant différer de la facture.",
        "Custo registrado pela ferramenta; pode diferir da fatura.",
        "Vom Tool gemeldete Kosten; die Rechnung kann abweichen.",
        "Стоимость по данным инструмента; счёт может отличаться.",
    ][language as usize]
}
fn unavailable_hint(language: Language) -> &'static str {
    [
        "비용 기록이나 확인된 단가·토큰 구분이 없어 계산할 수 없습니다.",
        "No usable cost record or verified model rate and token breakdown is available.",
        "コスト記録、確認済み単価、またはトークン内訳がないため計算できません。",
        "Faltan el coste, la tarifa verificada o el desglose de tokens.",
        "Coût, tarif vérifié ou détail des tokens indisponible.",
        "Faltam custo, tarifa verificada ou detalhamento de tokens.",
        "Kostenangabe, bestätigter Preis oder Token-Aufteilung fehlen.",
        "Нет записи о стоимости, а также проверенного тарифа модели и разбивки по токенам.",
    ][language as usize]
}
fn partial_hint(language: Language) -> &'static str {
    [
        "일부 사용량은 계산할 수 없어 제외했습니다. 표시 금액은 계산 가능한 부분의 합계이며 청구액이 아닙니다.",
        "Some usage could not be priced and is excluded. This is the known portion of usage cost, not an invoice.",
        "計算できない使用量を除いた部分合計です。請求額ではありません。",
        "Total parcial: excluye uso sin precio; no es una factura.",
        "Total partiel hors usage non chiffrable, pas une facture.",
        "Total parcial sem o uso não calculável; não é uma fatura.",
        "Teilsumme ohne nicht berechenbare Nutzung, keine Rechnung.",
        "Часть расхода не удалось оценить, она исключена. Показана стоимость только известной части — это не счёт.",
    ][language as usize]
}
impl<'de> Deserialize<'de> for Bucket {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let get = |key: &str, alias: &str| {
            v.get(key)
                .or_else(|| v.get(alias))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let cost = v
            .get("cost")
            .or_else(|| v.get("totalCost"))
            .or_else(|| v.get("costUSD"))
            .and_then(|v| v.as_f64())
            .filter(|c| c.is_finite() && *c >= 0.)
            .unwrap_or(0.);
        let mut out = Bucket {
            input: get("input", "inputTokens"),
            output: get("output", "outputTokens"),
            cache_write: get("cache_write", "cacheCreationTokens"),
            cache_read: get("cache_read", "cacheReadTokens"),
            cost,
            ..Bucket::default()
        };
        if out.total() == 0 {
            out.input = get("totalTokens", "total_tokens")
        }
        out.coverage = if let Some(coverage) = v.get("coverage").or_else(|| v.get("costCoverage")) {
            serde_json::from_value(coverage.clone()).map_err(serde::de::Error::custom)?
        } else if cost > 0. {
            CostCoverage {
                estimated: true,
                ..CostCoverage::default()
            }
        } else if out.total() > 0 {
            CostCoverage {
                unknown: true,
                ..CostCoverage::default()
            }
        } else {
            CostCoverage::default()
        };
        Ok(out)
    }
}
