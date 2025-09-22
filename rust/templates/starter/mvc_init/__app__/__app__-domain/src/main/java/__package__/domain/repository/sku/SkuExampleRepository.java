package __package__.domain.repository.sku;

import __package__.domain.model.sku.SkuDO;

/**
 * SkuExampleRepository
 *
 * @author weirenyan
 * @date 2024/5/8
 */
public interface SkuExampleRepository {

    /**
     * 获取SKU
     *
     * @param skuId
     * @return
     */
    SkuDO getSku(long skuId);

}
